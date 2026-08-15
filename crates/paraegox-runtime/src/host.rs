use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use paraegox_deck::CardKey;
use tokio::time::timeout;

use crate::BoxError;
use crate::deck::{DeckLaunch, DeckOwner};
use crate::status::{
    CardInstanceId, CardInstanceSnapshot, CardInstanceState, DeckRunId, DeckRunSnapshot,
    DeckRunState, RuntimeHostIdentity, RuntimeHostSnapshot, RuntimeHostState, RuntimeStatusReader,
};

/// The construction-time ceiling for CoreServices owned by one RuntimeHost.
pub const MAX_CORE_SERVICES: usize = 16;

const OWNER_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(10);
const FIRST_GENERATION: u64 = 1;

/// A CoreService must clean up its own partial resources before returning a
/// start error. `stop` must also tolerate an interrupted start and must not
/// return until owned work is joined.
#[async_trait]
pub trait CoreService: Send {
    async fn start(&mut self, runtime: RuntimeStatusReader) -> Result<(), BoxError>;

    async fn stop(&mut self) -> Result<(), BoxError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeHostBuildError {
    TooManyCoreServices { actual: usize, maximum: usize },
}

impl Display for RuntimeHostBuildError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooManyCoreServices { actual, maximum } => write!(
                formatter,
                "RuntimeHost has {actual} CoreServices; maximum is {maximum}"
            ),
        }
    }
}

impl Error for RuntimeHostBuildError {}

pub struct RuntimeHost {
    status: RuntimeStatusReader,
    core_services: Vec<Box<dyn CoreService>>,
    deck: Option<DeckOwner>,
    lifecycle_timeout: Duration,
}

impl RuntimeHost {
    pub fn new(
        identity: RuntimeHostIdentity,
        core_services: Vec<Box<dyn CoreService>>,
    ) -> Result<Self, RuntimeHostBuildError> {
        Self::build(identity, core_services, None)
    }

    pub fn with_deck(
        identity: RuntimeHostIdentity,
        core_services: Vec<Box<dyn CoreService>>,
        deck_launch: DeckLaunch,
    ) -> Result<Self, RuntimeHostBuildError> {
        Self::build(identity, core_services, Some(deck_launch))
    }

    fn build(
        identity: RuntimeHostIdentity,
        core_services: Vec<Box<dyn CoreService>>,
        deck_launch: Option<DeckLaunch>,
    ) -> Result<Self, RuntimeHostBuildError> {
        if core_services.len() > MAX_CORE_SERVICES {
            return Err(RuntimeHostBuildError::TooManyCoreServices {
                actual: core_services.len(),
                maximum: MAX_CORE_SERVICES,
            });
        }

        let (deck, deck_run) = if let Some(deck_launch) = deck_launch {
            let DeckLaunch {
                lock,
                implementations,
            } = deck_launch;
            let deck_run_id = DeckRunId::new();
            let card_instance_ids: Vec<_> =
                lock.cards().iter().map(|_| CardInstanceId::new()).collect();
            let cards = lock
                .cards()
                .iter()
                .zip(card_instance_ids.iter())
                .map(|(card, card_instance_id)| CardInstanceSnapshot {
                    card_instance_id: *card_instance_id,
                    key: card.key().clone(),
                    definition: card.definition().clone(),
                    generation: FIRST_GENERATION,
                    state: CardInstanceState::Created,
                })
                .collect();
            let deck_run = DeckRunSnapshot {
                deck_run_id,
                deck_key: lock.key().clone(),
                lock_digest: lock.digest().to_owned(),
                generation: FIRST_GENERATION,
                state: DeckRunState::Created,
                cards,
            };
            let owner = DeckOwner {
                lock,
                implementations,
                card_instance_ids,
            };
            (Some(owner), Some(deck_run))
        } else {
            (None, None)
        };

        Ok(Self {
            status: RuntimeStatusReader {
                snapshot: Arc::new(RwLock::new(RuntimeHostSnapshot {
                    identity,
                    state: RuntimeHostState::Created,
                    deck_run,
                })),
            },
            core_services,
            deck,
            lifecycle_timeout: OWNER_LIFECYCLE_TIMEOUT,
        })
    }

    pub fn snapshot(&self) -> RuntimeHostSnapshot {
        self.status.snapshot()
    }

    pub async fn start(&mut self) -> Result<(), RuntimeHostError> {
        self.require_state("start", RuntimeHostState::Created)?;
        self.set_state(RuntimeHostState::Starting);

        let mut started_services = 0;
        for index in 0..self.core_services.len() {
            let owner = LifecycleOwner::CoreService { index };
            let result = invoke_owner(
                owner.clone(),
                LifecycleAction::Start,
                self.lifecycle_timeout,
                self.core_services[index].start(self.status.clone()),
            )
            .await;

            if let Err(failure) = result {
                let mut cleanup_failures = Vec::new();
                if failure.is_timeout()
                    && let Err(cleanup_failure) = invoke_owner(
                        owner,
                        LifecycleAction::Stop,
                        self.lifecycle_timeout,
                        self.core_services[index].stop(),
                    )
                    .await
                {
                    cleanup_failures.push(cleanup_failure);
                }
                cleanup_failures.extend(
                    stop_started_services(
                        &mut self.core_services,
                        started_services,
                        self.lifecycle_timeout,
                    )
                    .await,
                );
                self.status.set_deck_run_state(DeckRunState::Failed);
                self.set_state(RuntimeHostState::Failed);
                return Err(RuntimeHostError::StartFailed {
                    failure,
                    cleanup_failures,
                });
            }

            started_services += 1;
        }

        let mut deck_start_failure = None;
        if let Some(deck) = self.deck.as_mut() {
            self.status.set_deck_run_state(DeckRunState::Starting);
            let mut started_cards = 0;

            for index in 0..deck.implementations.len() {
                let key = deck.lock.cards()[index].key().clone();
                let owner = LifecycleOwner::Card { key };
                let context = deck.context(index);
                self.status
                    .set_card_state(index, CardInstanceState::Starting);
                let result = invoke_owner(
                    owner.clone(),
                    LifecycleAction::Start,
                    self.lifecycle_timeout,
                    deck.implementations[index].start(context),
                )
                .await;

                match result {
                    Ok(()) => {
                        self.status.set_card_state(index, CardInstanceState::Ready);
                        started_cards += 1;
                    }
                    Err(failure) => {
                        self.status.set_card_state(index, CardInstanceState::Failed);
                        let mut cleanup_failures = Vec::new();
                        if failure.is_timeout() {
                            self.status
                                .set_card_state(index, CardInstanceState::Stopping);
                            match invoke_owner(
                                owner,
                                LifecycleAction::Stop,
                                self.lifecycle_timeout,
                                deck.implementations[index].stop(),
                            )
                            .await
                            {
                                Ok(()) => self
                                    .status
                                    .set_card_state(index, CardInstanceState::Stopped),
                                Err(cleanup_failure) => {
                                    self.status.set_card_state(index, CardInstanceState::Failed);
                                    cleanup_failures.push(cleanup_failure);
                                }
                            }
                        }
                        cleanup_failures.extend(
                            stop_started_cards(
                                deck,
                                started_cards,
                                &self.status,
                                self.lifecycle_timeout,
                            )
                            .await,
                        );
                        self.status.set_deck_run_state(DeckRunState::Failed);
                        deck_start_failure = Some((failure, cleanup_failures));
                        break;
                    }
                }
            }

            if deck_start_failure.is_none() {
                self.status.set_deck_run_state(DeckRunState::Ready);
            }
        }

        if let Some((failure, mut cleanup_failures)) = deck_start_failure {
            cleanup_failures.extend(
                stop_started_services(
                    &mut self.core_services,
                    started_services,
                    self.lifecycle_timeout,
                )
                .await,
            );
            self.set_state(RuntimeHostState::Failed);
            return Err(RuntimeHostError::StartFailed {
                failure,
                cleanup_failures,
            });
        }

        self.set_state(RuntimeHostState::Ready);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<(), RuntimeHostError> {
        self.require_state("stop", RuntimeHostState::Ready)?;
        self.set_state(RuntimeHostState::Stopping);

        let mut failures = Vec::new();
        if let Some(deck) = self.deck.as_mut() {
            self.status.set_deck_run_state(DeckRunState::Stopping);
            let started_cards = deck.implementations.len();
            let card_failures =
                stop_started_cards(deck, started_cards, &self.status, self.lifecycle_timeout).await;
            self.status.set_deck_run_state(if card_failures.is_empty() {
                DeckRunState::Stopped
            } else {
                DeckRunState::Failed
            });
            failures.extend(card_failures);
        }

        let started_services = self.core_services.len();
        failures.extend(
            stop_started_services(
                &mut self.core_services,
                started_services,
                self.lifecycle_timeout,
            )
            .await,
        );

        if failures.is_empty() {
            self.set_state(RuntimeHostState::Stopped);
            Ok(())
        } else {
            self.set_state(RuntimeHostState::Failed);
            Err(RuntimeHostError::StopFailed { failures })
        }
    }

    fn require_state(
        &self,
        action: &'static str,
        expected: RuntimeHostState,
    ) -> Result<(), RuntimeHostError> {
        let actual = self.snapshot().state;
        if actual == expected {
            Ok(())
        } else {
            Err(RuntimeHostError::InvalidTransition { action, actual })
        }
    }

    fn set_state(&self, state: RuntimeHostState) {
        self.status.set_runtime_host_state(state);
    }

    #[cfg(test)]
    pub(crate) fn with_lifecycle_timeout(
        identity: RuntimeHostIdentity,
        core_services: Vec<Box<dyn CoreService>>,
        lifecycle_timeout: Duration,
    ) -> Result<Self, RuntimeHostBuildError> {
        let mut runtime = Self::new(identity, core_services)?;
        runtime.lifecycle_timeout = lifecycle_timeout;
        Ok(runtime)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleOwner {
    CoreService { index: usize },
    Card { key: CardKey },
}

impl Display for LifecycleOwner {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::CoreService { index } => write!(formatter, "CoreService[{index}]"),
            Self::Card { key } => write!(formatter, "Card[{key}]"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleAction {
    Start,
    Stop,
}

impl Display for LifecycleAction {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Start => "start",
            Self::Stop => "stop",
        })
    }
}

#[derive(Debug)]
pub enum LifecycleFailureKind {
    TimedOut { deadline: Duration },
    Owner(BoxError),
}

#[derive(Debug)]
pub struct LifecycleFailure {
    pub owner: LifecycleOwner,
    pub action: LifecycleAction,
    pub kind: LifecycleFailureKind,
}

impl LifecycleFailure {
    fn is_timeout(&self) -> bool {
        matches!(self.kind, LifecycleFailureKind::TimedOut { .. })
    }
}

impl Display for LifecycleFailure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match &self.kind {
            LifecycleFailureKind::TimedOut { deadline } => write!(
                formatter,
                "{} {} exceeded the RuntimeHost deadline of {deadline:?}",
                self.owner, self.action
            ),
            LifecycleFailureKind::Owner(source) => {
                write!(formatter, "{} {} failed: {source}", self.owner, self.action)
            }
        }
    }
}

impl Error for LifecycleFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match &self.kind {
            LifecycleFailureKind::TimedOut { .. } => None,
            LifecycleFailureKind::Owner(source) => Some(source.as_ref()),
        }
    }
}

#[derive(Debug)]
pub enum RuntimeHostError {
    InvalidTransition {
        action: &'static str,
        actual: RuntimeHostState,
    },
    StartFailed {
        failure: LifecycleFailure,
        cleanup_failures: Vec<LifecycleFailure>,
    },
    StopFailed {
        failures: Vec<LifecycleFailure>,
    },
}

impl Display for RuntimeHostError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTransition { action, actual } => {
                write!(
                    formatter,
                    "cannot {action} RuntimeHost while it is {actual}"
                )
            }
            Self::StartFailed {
                failure,
                cleanup_failures,
            } if cleanup_failures.is_empty() => {
                write!(formatter, "RuntimeHost start failed: {failure}")
            }
            Self::StartFailed {
                failure,
                cleanup_failures,
            } => write!(
                formatter,
                "RuntimeHost start failed: {failure}; {} cleanup failure(s)",
                cleanup_failures.len()
            ),
            Self::StopFailed { failures } => write!(
                formatter,
                "RuntimeHost stop had {} lifecycle failure(s)",
                failures.len()
            ),
        }
    }
}

impl Error for RuntimeHostError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidTransition { .. } => None,
            Self::StartFailed { failure, .. } => Some(failure),
            Self::StopFailed { failures } => failures.first().map(|failure| failure as _),
        }
    }
}

async fn invoke_owner<FutureResult>(
    owner: LifecycleOwner,
    action: LifecycleAction,
    deadline: Duration,
    future: FutureResult,
) -> Result<(), LifecycleFailure>
where
    FutureResult: Future<Output = Result<(), BoxError>>,
{
    match timeout(deadline, future).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(source)) => Err(LifecycleFailure {
            owner,
            action,
            kind: LifecycleFailureKind::Owner(source),
        }),
        Err(_) => Err(LifecycleFailure {
            owner,
            action,
            kind: LifecycleFailureKind::TimedOut { deadline },
        }),
    }
}

async fn stop_started_services(
    services: &mut [Box<dyn CoreService>],
    started: usize,
    deadline: Duration,
) -> Vec<LifecycleFailure> {
    let mut failures = Vec::new();
    for index in (0..started).rev() {
        if let Err(failure) = invoke_owner(
            LifecycleOwner::CoreService { index },
            LifecycleAction::Stop,
            deadline,
            services[index].stop(),
        )
        .await
        {
            failures.push(failure);
        }
    }
    failures
}

async fn stop_started_cards(
    deck: &mut DeckOwner,
    started: usize,
    status: &RuntimeStatusReader,
    deadline: Duration,
) -> Vec<LifecycleFailure> {
    let mut failures = Vec::new();
    for index in (0..started).rev() {
        status.set_card_state(index, CardInstanceState::Stopping);
        let result = invoke_owner(
            LifecycleOwner::Card {
                key: deck.lock.cards()[index].key().clone(),
            },
            LifecycleAction::Stop,
            deadline,
            deck.implementations[index].stop(),
        )
        .await;
        match result {
            Ok(()) => status.set_card_state(index, CardInstanceState::Stopped),
            Err(failure) => {
                status.set_card_state(index, CardInstanceState::Failed);
                failures.push(failure);
            }
        }
    }
    failures
}
