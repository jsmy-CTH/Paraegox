//! RuntimeHost owns the bounded CoreService set and optional Deck run on one Node.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use paraegox_deck::{CardDefinitionRef, CardKey, DeckKey, DeckLock};
use paraegox_kernel::{RuntimeHostEpoch, RuntimeHostId};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use uuid::Uuid;

pub type BoxError = Box<dyn Error + Send + Sync + 'static>;

/// The construction-time ceiling for CoreServices owned by one RuntimeHost.
pub const MAX_CORE_SERVICES: usize = 16;

const OWNER_LIFECYCLE_TIMEOUT: Duration = Duration::from_secs(10);
const FIRST_GENERATION: u64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeHostState {
    Created,
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

impl Display for RuntimeHostState {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Created => "created",
            Self::Starting => "starting",
            Self::Ready => "ready",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeHostIdentity {
    pub runtime_host_id: RuntimeHostId,
    pub epoch: RuntimeHostEpoch,
}

impl RuntimeHostIdentity {
    pub fn new(runtime_host_id: RuntimeHostId) -> Self {
        Self {
            runtime_host_id,
            epoch: RuntimeHostEpoch::new(),
        }
    }
}

macro_rules! runtime_uuid {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                Display::fmt(&self.0, formatter)
            }
        }
    };
}

runtime_uuid!(DeckRunId);
runtime_uuid!(CardInstanceId);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeckRunState {
    Created,
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardInstanceState {
    Created,
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CardInstanceSnapshot {
    pub card_instance_id: CardInstanceId,
    pub key: CardKey,
    pub definition: CardDefinitionRef,
    pub generation: u64,
    pub state: CardInstanceState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeckRunSnapshot {
    pub deck_run_id: DeckRunId,
    pub deck_key: DeckKey,
    pub lock_digest: String,
    pub generation: u64,
    pub state: DeckRunState,
    pub cards: Vec<CardInstanceSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeHostSnapshot {
    pub identity: RuntimeHostIdentity,
    pub state: RuntimeHostState,
    pub deck_run: Option<DeckRunSnapshot>,
}

#[derive(Clone)]
pub struct RuntimeStatusReader {
    snapshot: Arc<RwLock<RuntimeHostSnapshot>>,
}

impl RuntimeStatusReader {
    pub fn snapshot(&self) -> RuntimeHostSnapshot {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn set_runtime_host_state(&self, state: RuntimeHostState) {
        self.snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .state = state;
    }

    fn set_deck_run_state(&self, state: DeckRunState) {
        if let Some(deck_run) = self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .deck_run
            .as_mut()
        {
            deck_run.state = state;
        }
    }

    fn set_card_state(&self, index: usize, state: CardInstanceState) {
        if let Some(card) = self
            .snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .deck_run
            .as_mut()
            .and_then(|deck_run| deck_run.cards.get_mut(index))
        {
            card.state = state;
        }
    }
}

/// A CoreService must clean up its own partial resources before returning a
/// start error. `stop` must also tolerate an interrupted start and must not
/// return until owned work is joined.
#[async_trait]
pub trait CoreService: Send {
    async fn start(&mut self, runtime: RuntimeStatusReader) -> Result<(), BoxError>;

    async fn stop(&mut self) -> Result<(), BoxError>;
}

/// The immutable identity and runtime-owned instance context for one Card start.
#[derive(Clone)]
pub struct CardContext {
    pub card_instance_id: CardInstanceId,
    pub card_key: CardKey,
    pub definition: CardDefinitionRef,
}

/// One exact implementation bound to one resolved Card in a DeckLock.
///
/// A start error must leave no partial resources behind. `stop` must tolerate
/// an interrupted start and must join all work owned by the Card instance.
#[async_trait]
pub trait CardImplementation: Send {
    fn card_key(&self) -> &CardKey;

    fn definition(&self) -> &CardDefinitionRef;

    async fn start(&mut self, context: CardContext) -> Result<(), BoxError>;

    async fn stop(&mut self) -> Result<(), BoxError>;
}

/// A validated, construction-time Deck workload and its exact implementations.
pub struct DeckLaunch {
    lock: DeckLock,
    implementations: Vec<Box<dyn CardImplementation>>,
}

impl DeckLaunch {
    pub fn new(
        lock: DeckLock,
        implementations: Vec<Box<dyn CardImplementation>>,
    ) -> Result<Self, DeckLaunchError> {
        if implementations.len() != lock.cards().len() {
            return Err(DeckLaunchError::ImplementationCount {
                expected: lock.cards().len(),
                actual: implementations.len(),
            });
        }

        for (index, (card, implementation)) in
            lock.cards().iter().zip(implementations.iter()).enumerate()
        {
            if implementation.card_key() != card.key()
                || implementation.definition() != card.definition()
            {
                return Err(DeckLaunchError::ImplementationMismatch {
                    index,
                    expected_key: card.key().clone(),
                    expected_definition: card.definition().clone(),
                    actual_key: implementation.card_key().clone(),
                    actual_definition: implementation.definition().clone(),
                });
            }
        }

        Ok(Self {
            lock,
            implementations,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeckLaunchError {
    ImplementationCount {
        expected: usize,
        actual: usize,
    },
    ImplementationMismatch {
        index: usize,
        expected_key: CardKey,
        expected_definition: CardDefinitionRef,
        actual_key: CardKey,
        actual_definition: CardDefinitionRef,
    },
}

impl Display for DeckLaunchError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ImplementationCount { expected, actual } => write!(
                formatter,
                "DeckLaunch needs {expected} Card implementations, got {actual}"
            ),
            Self::ImplementationMismatch {
                index,
                expected_key,
                expected_definition,
                actual_key,
                actual_definition,
            } => write!(
                formatter,
                "Card implementation {index} is `{actual_key}` / `{actual_definition}`; expected `{expected_key}` / `{expected_definition}`"
            ),
        }
    }
}

impl Error for DeckLaunchError {}

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

struct DeckOwner {
    lock: DeckLock,
    implementations: Vec<Box<dyn CardImplementation>>,
    card_instance_ids: Vec<CardInstanceId>,
}

impl DeckOwner {
    fn context(&self, index: usize) -> CardContext {
        let card = &self.lock.cards()[index];
        CardContext {
            card_instance_id: self.card_instance_ids[index],
            card_key: card.key().clone(),
            definition: card.definition().clone(),
        }
    }
}

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
    fn with_lifecycle_timeout(
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

#[cfg(test)]
mod tests {
    use std::future;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use async_trait::async_trait;
    use paraegox_deck::{Card, CardDefinitionRef, CardKey, DeckCompiler, DeckKey, DeckSpec};
    use paraegox_kernel::RuntimeHostId;

    use super::{
        BoxError, CardContext, CardImplementation, CardInstanceState, CoreService, DeckLaunch,
        DeckLaunchError, DeckRunState, LifecycleFailureKind, LifecycleOwner, RuntimeHost,
        RuntimeHostError, RuntimeHostIdentity, RuntimeHostState,
    };

    type Events = Arc<Mutex<Vec<String>>>;

    struct RecordingService {
        label: &'static str,
        events: Events,
        fail_on_stop: bool,
        hang_on_stop: bool,
    }

    #[async_trait]
    impl CoreService for RecordingService {
        async fn start(&mut self, runtime: super::RuntimeStatusReader) -> Result<(), BoxError> {
            if runtime.snapshot().state != RuntimeHostState::Starting {
                return Err(io::Error::other("service did not observe Starting").into());
            }
            record(&self.events, format!("{}:start", self.label));
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), BoxError> {
            record(&self.events, format!("{}:stop", self.label));
            if self.hang_on_stop {
                future::pending::<()>().await;
            }
            if self.fail_on_stop {
                return Err(io::Error::other(format!("{} stop failed", self.label)).into());
            }
            Ok(())
        }
    }

    struct RecordingCard {
        key: CardKey,
        definition: CardDefinitionRef,
        label: &'static str,
        events: Events,
        fail_on_start: bool,
    }

    #[async_trait]
    impl CardImplementation for RecordingCard {
        fn card_key(&self) -> &CardKey {
            &self.key
        }

        fn definition(&self) -> &CardDefinitionRef {
            &self.definition
        }

        async fn start(&mut self, context: CardContext) -> Result<(), BoxError> {
            if context.card_key != self.key || context.definition != self.definition {
                return Err(io::Error::other("Card received the wrong start context").into());
            }
            record(&self.events, format!("{}:start", self.label));
            if self.fail_on_start {
                return Err(io::Error::other(format!("{} start failed", self.label)).into());
            }
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), BoxError> {
            record(&self.events, format!("{}:stop", self.label));
            Ok(())
        }
    }

    #[test]
    fn deck_launch_rejects_an_identity_mismatch_before_runtime_start() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let lock = deck_lock(&[("agent", "builtin.agent@1")]);
        let implementation = RecordingCard {
            key: CardKey::new("wrong-key"),
            definition: CardDefinitionRef::new("builtin.agent@1"),
            label: "wrong",
            events: Arc::clone(&events),
            fail_on_start: false,
        };

        let result = DeckLaunch::new(lock, vec![Box::new(implementation)]);
        assert!(matches!(
            result,
            Err(DeckLaunchError::ImplementationMismatch { index: 0, .. })
        ));
        assert_events(&events, &[]);
    }

    #[tokio::test]
    async fn runtime_orders_multiple_services_before_cards_and_stops_in_reverse() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let lock = deck_lock(&[
            ("agent", "builtin.agent@1"),
            ("terminal", "builtin.terminal@1"),
        ]);
        let launch = DeckLaunch::new(
            lock,
            vec![
                card("agent", "builtin.agent@1", "card-agent", &events, false),
                card(
                    "terminal",
                    "builtin.terminal@1",
                    "card-terminal",
                    &events,
                    false,
                ),
            ],
        )
        .expect("matching DeckLaunch");
        let mut runtime = RuntimeHost::with_deck(
            runtime_identity(),
            vec![
                service("service-a", &events, false, false),
                service("service-b", &events, false, false),
            ],
            launch,
        )
        .expect("bounded RuntimeHost");

        runtime.start().await.expect("runtime should start");
        let ready = runtime.snapshot();
        let ready_deck = ready.deck_run.expect("Deck run should be observable");
        assert_eq!(ready.state, RuntimeHostState::Ready);
        assert_eq!(ready_deck.generation, 1);
        assert_eq!(ready_deck.state, DeckRunState::Ready);
        assert!(
            ready_deck
                .cards
                .iter()
                .all(|card| { card.generation == 1 && card.state == CardInstanceState::Ready })
        );
        assert_ne!(
            ready_deck.cards[0].card_instance_id,
            ready_deck.cards[1].card_instance_id
        );

        runtime.stop().await.expect("runtime should stop");
        let stopped = runtime.snapshot();
        let stopped_deck = stopped.deck_run.expect("Deck run remains observable");
        assert_eq!(stopped.state, RuntimeHostState::Stopped);
        assert_eq!(stopped_deck.deck_run_id, ready_deck.deck_run_id);
        assert_eq!(stopped_deck.state, DeckRunState::Stopped);
        assert!(
            stopped_deck
                .cards
                .iter()
                .all(|card| card.state == CardInstanceState::Stopped)
        );
        assert_events(
            &events,
            &[
                "service-a:start",
                "service-b:start",
                "card-agent:start",
                "card-terminal:start",
                "card-terminal:stop",
                "card-agent:stop",
                "service-b:stop",
                "service-a:stop",
            ],
        );
    }

    #[tokio::test]
    async fn card_start_failure_rolls_back_cards_then_services_and_collects_cleanup_errors() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let lock = deck_lock(&[("first", "builtin.first@1"), ("second", "builtin.second@1")]);
        let launch = DeckLaunch::new(
            lock,
            vec![
                card("first", "builtin.first@1", "card-first", &events, false),
                card("second", "builtin.second@1", "card-second", &events, true),
            ],
        )
        .expect("matching DeckLaunch");
        let mut runtime = RuntimeHost::with_deck(
            runtime_identity(),
            vec![
                service("service-a", &events, false, false),
                service("service-b", &events, true, false),
            ],
            launch,
        )
        .expect("bounded RuntimeHost");

        let error = runtime.start().await.expect_err("second Card must fail");
        let RuntimeHostError::StartFailed {
            failure,
            cleanup_failures,
        } = error
        else {
            panic!("unexpected RuntimeHost error")
        };
        assert_eq!(
            failure.owner,
            LifecycleOwner::Card {
                key: CardKey::new("second")
            }
        );
        assert!(matches!(failure.kind, LifecycleFailureKind::Owner(_)));
        assert_eq!(cleanup_failures.len(), 1);
        assert_eq!(
            cleanup_failures[0].owner,
            LifecycleOwner::CoreService { index: 1 }
        );

        let failed = runtime.snapshot();
        let failed_deck = failed.deck_run.expect("failed Deck remains observable");
        assert_eq!(failed.state, RuntimeHostState::Failed);
        assert_eq!(failed_deck.state, DeckRunState::Failed);
        assert_eq!(failed_deck.cards[0].state, CardInstanceState::Stopped);
        assert_eq!(failed_deck.cards[1].state, CardInstanceState::Failed);
        assert_events(
            &events,
            &[
                "service-a:start",
                "service-b:start",
                "card-first:start",
                "card-second:start",
                "card-first:stop",
                "service-b:stop",
                "service-a:stop",
            ],
        );
    }

    #[tokio::test]
    async fn a_hung_owner_lifecycle_is_bounded_by_the_runtime_deadline() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut runtime = RuntimeHost::with_lifecycle_timeout(
            runtime_identity(),
            vec![service("hung", &events, false, true)],
            Duration::from_millis(20),
        )
        .expect("bounded RuntimeHost");
        runtime.start().await.expect("runtime should start");

        let error = tokio::time::timeout(Duration::from_secs(1), runtime.stop())
            .await
            .expect("RuntimeHost must enforce its own deadline")
            .expect_err("hung stop must fail");
        let RuntimeHostError::StopFailed { failures } = error else {
            panic!("unexpected RuntimeHost error")
        };
        assert_eq!(failures.len(), 1);
        assert!(matches!(
            &failures[0].kind,
            LifecycleFailureKind::TimedOut { .. }
        ));
        assert_eq!(runtime.snapshot().state, RuntimeHostState::Failed);
        assert_events(&events, &["hung:start", "hung:stop"]);
    }

    fn runtime_identity() -> RuntimeHostIdentity {
        RuntimeHostIdentity::new(RuntimeHostId::new("runtime-test").expect("valid RuntimeHost id"))
    }

    fn service(
        label: &'static str,
        events: &Events,
        fail_on_stop: bool,
        hang_on_stop: bool,
    ) -> Box<dyn CoreService> {
        Box::new(RecordingService {
            label,
            events: Arc::clone(events),
            fail_on_stop,
            hang_on_stop,
        })
    }

    fn card(
        key: &str,
        definition: &str,
        label: &'static str,
        events: &Events,
        fail_on_start: bool,
    ) -> Box<dyn CardImplementation> {
        Box::new(RecordingCard {
            key: CardKey::new(key),
            definition: CardDefinitionRef::new(definition),
            label,
            events: Arc::clone(events),
            fail_on_start,
        })
    }

    fn deck_lock(cards: &[(&str, &str)]) -> paraegox_deck::DeckLock {
        let definitions = cards
            .iter()
            .map(|(_, definition)| CardDefinitionRef::new(*definition));
        let compiler = DeckCompiler::new(definitions).expect("valid definitions");
        compiler
            .compile(&DeckSpec {
                key: DeckKey::new("test-deck"),
                cards: cards
                    .iter()
                    .map(|(key, definition)| Card {
                        key: CardKey::new(*key),
                        definition: CardDefinitionRef::new(*definition),
                    })
                    .collect(),
            })
            .expect("valid Deck")
    }

    fn record(events: &Events, event: String) {
        events.lock().expect("events lock").push(event);
    }

    fn assert_events(events: &Events, expected: &[&str]) {
        let actual = events.lock().expect("events lock");
        assert_eq!(
            actual.iter().map(String::as_str).collect::<Vec<_>>(),
            expected
        );
    }
}
