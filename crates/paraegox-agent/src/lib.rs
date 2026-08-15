//! Agent CoreService, built-in Agent Card, and typed conversation client.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use paraegox_deck::{CardDefinitionRef, CardKey};
use paraegox_fabric::{FabricClient, FabricQueryBinding};
use paraegox_kernel::NodeId;
use paraegox_runtime::{
    BoxError, CardContext, CardImplementation, CardInstanceId, CoreService, RuntimeStatusReader,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

pub const BUILTIN_AGENT_DEFINITION: &str = "builtin.agent.deterministic@1";

const MAX_SESSIONS: usize = 64;
const MAX_RETAINED_TURNS_PER_SESSION: usize = 32;
const MAX_INPUT_BYTES: usize = 4 * 1024;
const MAX_TURN_DEADLINE: Duration = Duration::from_secs(60);
const CLIENT_REPLY_GRACE: Duration = Duration::from_millis(250);

pub fn builtin_agent_definition() -> CardDefinitionRef {
    CardDefinitionRef::new(BUILTIN_AGENT_DEFINITION)
}

macro_rules! agent_uuid {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl Display for $name {
            fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
                Display::fmt(&self.0, formatter)
            }
        }
    };
}

agent_uuid!(SessionId);
agent_uuid!(TurnId);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "terminal", rename_all = "snake_case")]
pub enum TurnTerminal {
    Final { content: String },
    Cancelled,
    TimedOut,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TurnResult {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    pub terminal: TurnTerminal,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CancelResult {
    CancellationRequested,
    AlreadyTerminal { terminal: TurnTerminal },
    NotActive,
}

pub struct AgentService {
    inner: Arc<AgentInner>,
}

#[derive(Clone)]
pub struct AgentHandle {
    inner: Weak<AgentInner>,
}

pub struct AgentCard {
    key: CardKey,
    definition: CardDefinitionRef,
    handle: AgentHandle,
    active_instance: Option<CardInstanceId>,
}

pub struct AgentConversationClient {
    fabric: FabricClient,
    binding_key: String,
    session_id: SessionId,
}

struct AgentInner {
    state: Mutex<AgentState>,
    idle: Notify,
    response_delay: Duration,
}

struct AgentState {
    lifecycle: ServiceLifecycle,
    active_profile: Option<CardInstanceId>,
    sessions: HashMap<SessionId, SessionState>,
    session_order: VecDeque<SessionId>,
    active_turn: Option<ActiveTurn>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ServiceLifecycle {
    Created,
    Running,
    Stopping,
    Stopped,
}

#[derive(Default)]
struct SessionState {
    turns: VecDeque<CompletedTurn>,
    pending_cancellations: VecDeque<TurnId>,
}

struct CompletedTurn {
    turn_id: TurnId,
    input: String,
    terminal: TurnTerminal,
}

struct ActiveTurn {
    session_id: SessionId,
    turn_id: TurnId,
    cancellation: watch::Sender<bool>,
}

struct ActiveTurnGuard {
    inner: Arc<AgentInner>,
    session_id: SessionId,
    turn_id: TurnId,
    input: String,
    armed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum WireRequest {
    Submit {
        session_id: SessionId,
        turn_id: TurnId,
        input: String,
        deadline_ms: u64,
    },
    Cancel {
        session_id: SessionId,
        turn_id: TurnId,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
enum WireResponse {
    Turn {
        result: TurnResult,
    },
    Cancel {
        session_id: SessionId,
        turn_id: TurnId,
        result: CancelResult,
    },
}

impl AgentService {
    pub fn new() -> Self {
        Self::with_response_delay(Duration::ZERO)
    }

    fn with_response_delay(response_delay: Duration) -> Self {
        Self {
            inner: Arc::new(AgentInner {
                state: Mutex::new(AgentState {
                    lifecycle: ServiceLifecycle::Created,
                    active_profile: None,
                    sessions: HashMap::new(),
                    session_order: VecDeque::new(),
                    active_turn: None,
                }),
                idle: Notify::new(),
                response_delay,
            }),
        }
    }

    pub fn handle(&self) -> AgentHandle {
        AgentHandle {
            inner: Arc::downgrade(&self.inner),
        }
    }

    async fn stop_inner(&self) -> Result<(), AgentError> {
        {
            let mut state = lock_state(&self.inner);
            match state.lifecycle {
                ServiceLifecycle::Created => {
                    state.lifecycle = ServiceLifecycle::Stopped;
                    return Ok(());
                }
                ServiceLifecycle::Running => state.lifecycle = ServiceLifecycle::Stopping,
                ServiceLifecycle::Stopping => {}
                ServiceLifecycle::Stopped => return Ok(()),
            }
            state.active_profile = None;
            cancel_active_turn(&state);
        }

        wait_until_idle(&self.inner).await;
        lock_state(&self.inner).lifecycle = ServiceLifecycle::Stopped;
        Ok(())
    }
}

impl Default for AgentService {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for AgentService {
    fn drop(&mut self) {
        let mut state = lock_state(&self.inner);
        state.active_profile = None;
        state.lifecycle = ServiceLifecycle::Stopped;
        cancel_active_turn(&state);
    }
}

#[async_trait]
impl CoreService for AgentService {
    async fn start(&mut self, _runtime: RuntimeStatusReader) -> Result<(), BoxError> {
        let mut state = lock_state(&self.inner);
        if state.lifecycle != ServiceLifecycle::Created {
            return Err(Box::new(AgentError::new(
                "AgentService can only start from Created",
            )));
        }
        state.lifecycle = ServiceLifecycle::Running;
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), BoxError> {
        self.stop_inner()
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

impl AgentHandle {
    async fn submit_turn(
        &self,
        session_id: SessionId,
        turn_id: TurnId,
        input: &str,
        deadline: Duration,
    ) -> Result<TurnResult, AgentError> {
        validate_input(input)?;
        validate_turn_deadline(deadline)?;
        let inner = self.upgrade()?;

        let (previous_input, mut cancellation) = {
            let mut state = lock_state(&inner);
            require_admission(&state)?;

            if let Some(session) = state.sessions.get(&session_id)
                && let Some(completed) = session.turns.iter().find(|turn| turn.turn_id == turn_id)
            {
                if completed.input != input {
                    return Err(AgentError::new(
                        "TurnId was already completed with different input",
                    ));
                }
                return Ok(TurnResult {
                    session_id,
                    turn_id,
                    terminal: completed.terminal.clone(),
                });
            }

            if let Some(session) = state.sessions.get_mut(&session_id)
                && let Some(position) = session
                    .pending_cancellations
                    .iter()
                    .position(|pending| *pending == turn_id)
            {
                session.pending_cancellations.remove(position);
                let terminal = TurnTerminal::Cancelled;
                record_completed_turn(session, turn_id, input, terminal.clone());
                return Ok(TurnResult {
                    session_id,
                    turn_id,
                    terminal,
                });
            }

            if state.active_turn.is_some() {
                return Err(AgentError::new("AgentService is busy with another turn"));
            }

            ensure_session(&mut state, session_id)?;
            ensure_turn_slot(&state, session_id)?;

            let previous_input = state.sessions.get(&session_id).and_then(|session| {
                session.turns.iter().rev().find_map(|turn| {
                    matches!(turn.terminal, TurnTerminal::Final { .. }).then(|| turn.input.clone())
                })
            });
            let (cancellation_sender, cancellation_receiver) = watch::channel(false);
            state.active_turn = Some(ActiveTurn {
                session_id,
                turn_id,
                cancellation: cancellation_sender,
            });
            (previous_input, cancellation_receiver)
        };
        let guard = ActiveTurnGuard::new(Arc::clone(&inner), session_id, turn_id, input);

        let response_delay = inner.response_delay;
        let terminal = tokio::select! {
            () = wait_for_cancellation(&mut cancellation) => TurnTerminal::Cancelled,
            result = timeout(deadline, sleep(response_delay)) => match result {
                Ok(()) => TurnTerminal::Final {
                    content: deterministic_final(previous_input.as_deref(), input),
                },
                Err(_) => TurnTerminal::TimedOut,
            },
        };

        let terminal = guard.complete(terminal)?;

        Ok(TurnResult {
            session_id,
            turn_id,
            terminal,
        })
    }

    fn cancel(&self, session_id: SessionId, turn_id: TurnId) -> Result<CancelResult, AgentError> {
        let inner = self.upgrade()?;
        let mut state = lock_state(&inner);

        if let Some(completed) = state
            .sessions
            .get(&session_id)
            .and_then(|session| session.turns.iter().find(|turn| turn.turn_id == turn_id))
        {
            return Ok(CancelResult::AlreadyTerminal {
                terminal: completed.terminal.clone(),
            });
        }

        if let Some(active) = state.active_turn.as_ref()
            && active.session_id == session_id
            && active.turn_id == turn_id
        {
            let _ = active.cancellation.send(true);
            return Ok(CancelResult::CancellationRequested);
        }

        if state.lifecycle != ServiceLifecycle::Running || state.active_profile.is_none() {
            return Ok(CancelResult::NotActive);
        }
        ensure_session(&mut state, session_id)?;
        let already_pending = state
            .sessions
            .get(&session_id)
            .expect("a cancellation owns a bounded session")
            .pending_cancellations
            .contains(&turn_id);
        if !already_pending {
            ensure_turn_slot(&state, session_id)?;
            state
                .sessions
                .get_mut(&session_id)
                .expect("a cancellation owns a bounded session")
                .pending_cancellations
                .push_back(turn_id);
        }
        Ok(CancelResult::CancellationRequested)
    }

    fn activate_profile(&self, card_instance_id: CardInstanceId) -> Result<(), AgentError> {
        let inner = self.upgrade()?;
        let mut state = lock_state(&inner);
        if state.lifecycle != ServiceLifecycle::Running {
            return Err(AgentError::new("AgentService is not running"));
        }
        if state.active_profile.is_some() {
            return Err(AgentError::new("an Agent profile is already active"));
        }
        state.active_profile = Some(card_instance_id);
        Ok(())
    }

    async fn deactivate_profile(&self, card_instance_id: CardInstanceId) -> Result<(), AgentError> {
        let inner = self.upgrade()?;
        {
            let mut state = lock_state(&inner);
            match state.active_profile {
                Some(active) if active == card_instance_id => state.active_profile = None,
                Some(_) => {
                    return Err(AgentError::new(
                        "another Agent Card owns the active profile",
                    ));
                }
                None => return Ok(()),
            }
            cancel_active_turn(&state);
        }
        wait_until_idle(&inner).await;
        Ok(())
    }

    fn upgrade(&self) -> Result<Arc<AgentInner>, AgentError> {
        self.inner
            .upgrade()
            .ok_or_else(|| AgentError::new("AgentService owner is unavailable"))
    }
}

impl ActiveTurnGuard {
    fn new(inner: Arc<AgentInner>, session_id: SessionId, turn_id: TurnId, input: &str) -> Self {
        Self {
            inner,
            session_id,
            turn_id,
            input: input.to_owned(),
            armed: true,
        }
    }

    fn complete(mut self, proposed: TurnTerminal) -> Result<TurnTerminal, AgentError> {
        let terminal = {
            let mut state = lock_state(&self.inner);
            let Some(active) = state.active_turn.as_ref() else {
                return Err(AgentError::new(
                    "AgentService lost ownership of the active turn",
                ));
            };
            if active.session_id != self.session_id || active.turn_id != self.turn_id {
                return Err(AgentError::new(
                    "AgentService active turn identity changed unexpectedly",
                ));
            }
            let cancellation_requested = *active.cancellation.borrow();
            let terminal = if cancellation_requested
                || state.lifecycle != ServiceLifecycle::Running
                || state.active_profile.is_none()
            {
                TurnTerminal::Cancelled
            } else {
                proposed
            };
            state.active_turn = None;
            let session = state
                .sessions
                .get_mut(&self.session_id)
                .expect("an admitted turn always owns a session");
            record_completed_turn(session, self.turn_id, &self.input, terminal.clone());
            terminal
        };
        self.armed = false;
        self.inner.idle.notify_waiters();
        Ok(terminal)
    }
}

impl Drop for ActiveTurnGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        let cleared = {
            let mut state = lock_state(&self.inner);
            let active_matches = state.active_turn.as_ref().is_some_and(|active| {
                active.session_id == self.session_id && active.turn_id == self.turn_id
            });
            if !active_matches {
                false
            } else {
                state.active_turn = None;
                if let Some(session) = state.sessions.get_mut(&self.session_id) {
                    record_completed_turn(
                        session,
                        self.turn_id,
                        &self.input,
                        TurnTerminal::Cancelled,
                    );
                }
                true
            }
        };
        if cleared {
            self.inner.idle.notify_waiters();
        }
    }
}

impl AgentCard {
    pub fn new(key: CardKey, handle: AgentHandle) -> Self {
        Self {
            key,
            definition: builtin_agent_definition(),
            handle,
            active_instance: None,
        }
    }
}

#[async_trait]
impl CardImplementation for AgentCard {
    fn card_key(&self) -> &CardKey {
        &self.key
    }

    fn definition(&self) -> &CardDefinitionRef {
        &self.definition
    }

    async fn start(&mut self, context: CardContext) -> Result<(), BoxError> {
        if context.card_key != self.key || context.definition != self.definition {
            return Err(Box::new(AgentError::new(
                "Agent Card received mismatched Runtime context",
            )));
        }
        self.handle
            .activate_profile(context.card_instance_id)
            .map_err(|error| Box::new(error) as BoxError)?;
        self.active_instance = Some(context.card_instance_id);
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), BoxError> {
        let Some(card_instance_id) = self.active_instance.take() else {
            return Ok(());
        };
        self.handle
            .deactivate_profile(card_instance_id)
            .await
            .map_err(|error| Box::new(error) as BoxError)
    }
}

pub fn agent_query_binding(
    node_id: &NodeId,
    handle: AgentHandle,
) -> Result<FabricQueryBinding, AgentError> {
    FabricQueryBinding::new(agent_query_key(node_id), move |_runtime, payload| {
        let handle = handle.clone();
        async move {
            serve_wire_request(&handle, &payload)
                .await
                .map_err(|error| error.to_string())
        }
    })
    .map_err(|error| AgentError::context("could not create Agent Fabric binding", error))
}

impl AgentConversationClient {
    pub async fn connect(
        endpoint: &str,
        target: NodeId,
        session_id: SessionId,
        deadline: Duration,
    ) -> Result<Self, AgentError> {
        let fabric = FabricClient::connect(endpoint, deadline)
            .await
            .map_err(|error| AgentError::context("could not connect Agent client", error))?;
        Ok(Self {
            fabric,
            binding_key: agent_query_key(&target),
            session_id,
        })
    }

    pub async fn submit_turn(
        &self,
        turn_id: TurnId,
        input: &str,
        deadline: Duration,
    ) -> Result<TurnResult, AgentError> {
        validate_input(input)?;
        let deadline_ms = wire_deadline_ms(deadline)?;
        let request = WireRequest::Submit {
            session_id: self.session_id,
            turn_id,
            input: input.to_owned(),
            deadline_ms,
        };
        let response = self
            .request(&request, client_reply_deadline(deadline)?)
            .await?;
        match response {
            WireResponse::Turn { result }
                if result.session_id == self.session_id && result.turn_id == turn_id =>
            {
                Ok(result)
            }
            WireResponse::Turn { .. } => Err(AgentError::new(
                "Agent response identity does not match the submitted turn",
            )),
            WireResponse::Cancel { .. } => Err(AgentError::new(
                "Agent returned a cancel response for a submitted turn",
            )),
        }
    }

    pub async fn cancel(
        &self,
        turn_id: TurnId,
        deadline: Duration,
    ) -> Result<CancelResult, AgentError> {
        let request = WireRequest::Cancel {
            session_id: self.session_id,
            turn_id,
        };
        match self.request(&request, deadline).await? {
            WireResponse::Cancel {
                session_id,
                turn_id: response_turn,
                result,
            } if session_id == self.session_id && response_turn == turn_id => Ok(result),
            WireResponse::Cancel { .. } => Err(AgentError::new(
                "Agent cancel response identity does not match the request",
            )),
            WireResponse::Turn { .. } => Err(AgentError::new(
                "Agent returned a turn response for a cancel request",
            )),
        }
    }

    pub async fn close(&mut self, deadline: Duration) -> Result<(), AgentError> {
        self.fabric
            .close(deadline)
            .await
            .map_err(|error| AgentError::context("could not close Agent client", error))
    }

    async fn request(
        &self,
        request: &WireRequest,
        deadline: Duration,
    ) -> Result<WireResponse, AgentError> {
        let payload = serde_json::to_vec(request)
            .map_err(|error| AgentError::context("could not encode Agent request", error))?;
        let response = self
            .fabric
            .request_one(&self.binding_key, &payload, deadline)
            .await
            .map_err(|error| AgentError::context("Agent Fabric request failed", error))?;
        serde_json::from_slice(&response)
            .map_err(|error| AgentError::context("could not decode Agent response", error))
    }
}

async fn serve_wire_request(handle: &AgentHandle, payload: &[u8]) -> Result<Vec<u8>, AgentError> {
    let request: WireRequest = serde_json::from_slice(payload)
        .map_err(|error| AgentError::context("invalid Agent request", error))?;
    let response = match request {
        WireRequest::Submit {
            session_id,
            turn_id,
            input,
            deadline_ms,
        } => WireResponse::Turn {
            result: handle
                .submit_turn(
                    session_id,
                    turn_id,
                    &input,
                    Duration::from_millis(deadline_ms),
                )
                .await?,
        },
        WireRequest::Cancel {
            session_id,
            turn_id,
        } => WireResponse::Cancel {
            session_id,
            turn_id,
            result: handle.cancel(session_id, turn_id)?,
        },
    };
    serde_json::to_vec(&response)
        .map_err(|error| AgentError::context("could not encode Agent response", error))
}

fn agent_query_key(node_id: &NodeId) -> String {
    format!("paraegox/v1/nodes/{}/agent/conversation", node_id.as_str())
}

fn require_admission(state: &AgentState) -> Result<(), AgentError> {
    if state.lifecycle != ServiceLifecycle::Running {
        return Err(AgentError::new("AgentService is not running"));
    }
    if state.active_profile.is_none() {
        return Err(AgentError::new("no Agent Card is active"));
    }
    Ok(())
}

fn ensure_session(state: &mut AgentState, session_id: SessionId) -> Result<(), AgentError> {
    if state.sessions.contains_key(&session_id) {
        return Ok(());
    }

    if state.sessions.len() == MAX_SESSIONS {
        let protected = state.active_turn.as_ref().map(|active| active.session_id);
        let position = state
            .session_order
            .iter()
            .position(|candidate| Some(*candidate) != protected)
            .ok_or_else(|| AgentError::new("no inactive Agent session can be evicted"))?;
        let expired = state
            .session_order
            .remove(position)
            .expect("the selected session order entry exists");
        state.sessions.remove(&expired);
    }
    state.sessions.insert(session_id, SessionState::default());
    state.session_order.push_back(session_id);
    Ok(())
}

fn ensure_turn_slot(state: &AgentState, session_id: SessionId) -> Result<(), AgentError> {
    let session = state
        .sessions
        .get(&session_id)
        .expect("turn admission owns a bounded session");
    let active_reservation = usize::from(
        state
            .active_turn
            .as_ref()
            .is_some_and(|active| active.session_id == session_id),
    );
    if session.turns.len() + session.pending_cancellations.len() + active_reservation
        >= MAX_RETAINED_TURNS_PER_SESSION
    {
        return Err(AgentError::new(
            "Agent session terminal capacity is full; create a new Session",
        ));
    }
    Ok(())
}

fn record_completed_turn(
    session: &mut SessionState,
    turn_id: TurnId,
    input: &str,
    terminal: TurnTerminal,
) {
    debug_assert!(session.turns.len() < MAX_RETAINED_TURNS_PER_SESSION);
    session.turns.push_back(CompletedTurn {
        turn_id,
        input: input.to_owned(),
        terminal,
    });
}

fn validate_input(input: &str) -> Result<(), AgentError> {
    if input.trim().is_empty() {
        return Err(AgentError::new("Agent input must not be empty"));
    }
    if input.len() > MAX_INPUT_BYTES {
        return Err(AgentError::new("Agent input exceeds 4 KiB"));
    }
    if input.chars().any(char::is_control) {
        return Err(AgentError::new(
            "Agent input must not contain control characters",
        ));
    }
    Ok(())
}

fn validate_turn_deadline(deadline: Duration) -> Result<(), AgentError> {
    if deadline.is_zero() || deadline > MAX_TURN_DEADLINE {
        return Err(AgentError::new(
            "Agent turn deadline must be greater than zero and at most 60 seconds",
        ));
    }
    Ok(())
}

fn wire_deadline_ms(deadline: Duration) -> Result<u64, AgentError> {
    validate_turn_deadline(deadline)?;
    let milliseconds = u64::try_from(deadline.as_millis())
        .map_err(|_| AgentError::new("Agent turn deadline is too large"))?;
    if milliseconds == 0 {
        return Err(AgentError::new(
            "Agent client deadline must be at least one millisecond",
        ));
    }
    Ok(milliseconds)
}

fn client_reply_deadline(deadline: Duration) -> Result<Duration, AgentError> {
    deadline
        .checked_add(CLIENT_REPLY_GRACE)
        .ok_or_else(|| AgentError::new("Agent client reply deadline is too large"))
}

fn deterministic_final(previous_input: Option<&str>, input: &str) -> String {
    match previous_input {
        Some(previous) => format!("previous: {previous}; current: {input}"),
        None => format!("current: {input}"),
    }
}

fn cancel_active_turn(state: &AgentState) {
    if let Some(active) = state.active_turn.as_ref() {
        let _ = active.cancellation.send(true);
    }
}

async fn wait_for_cancellation(cancellation: &mut watch::Receiver<bool>) {
    loop {
        if *cancellation.borrow() {
            return;
        }
        if cancellation.changed().await.is_err() {
            return;
        }
    }
}

async fn wait_until_idle(inner: &AgentInner) {
    loop {
        let notified = inner.idle.notified();
        if lock_state(inner).active_turn.is_none() {
            return;
        }
        notified.await;
    }
}

fn lock_state(inner: &AgentInner) -> std::sync::MutexGuard<'_, AgentState> {
    inner
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Debug)]
pub struct AgentError {
    message: String,
}

impl AgentError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    fn context(context: &str, error: impl Display) -> Self {
        Self::new(format!("{context}: {error}"))
    }
}

impl Display for AgentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AgentError {}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use paraegox_deck::{Card, CardKey, DeckCompiler, DeckKey, DeckSpec};
    use paraegox_kernel::RuntimeHostId;
    use paraegox_runtime::{DeckLaunch, RuntimeHost, RuntimeHostIdentity};

    use super::*;

    #[tokio::test]
    async fn card_admission_history_idempotency_timeout_cancel_and_shutdown_are_bounded() {
        let no_card_service = AgentService::new();
        let no_card_handle = no_card_service.handle();
        let mut no_card_runtime = RuntimeHost::new(
            runtime_identity("agent-no-card"),
            vec![Box::new(no_card_service)],
        )
        .unwrap();
        no_card_runtime.start().await.unwrap();
        let rejected = serve_wire_request(
            &no_card_handle,
            &serde_json::to_vec(&WireRequest::Submit {
                session_id: SessionId::new(),
                turn_id: TurnId::new(),
                input: "hello".to_owned(),
                deadline_ms: 1_000,
            })
            .unwrap(),
        )
        .await
        .unwrap_err();
        assert_eq!(rejected.to_string(), "no Agent Card is active");
        no_card_runtime.stop().await.unwrap();

        let service = AgentService::with_response_delay(Duration::from_millis(40));
        let handle = service.handle();
        let key = CardKey::new("agent");
        let definition = builtin_agent_definition();
        let compiler = DeckCompiler::new([definition.clone()]).unwrap();
        let lock = compiler
            .compile(&DeckSpec {
                key: DeckKey::new("agent-test"),
                cards: vec![Card {
                    key: key.clone(),
                    definition,
                }],
            })
            .unwrap();
        let launch =
            DeckLaunch::new(lock, vec![Box::new(AgentCard::new(key, handle.clone()))]).unwrap();
        let mut runtime = RuntimeHost::with_deck(
            runtime_identity("agent-runtime"),
            vec![Box::new(service)],
            launch,
        )
        .unwrap();
        runtime.start().await.unwrap();

        let session = SessionId::new();
        let first_turn = TurnId::new();
        let first = handle
            .submit_turn(session, first_turn, "alpha", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            first.terminal,
            TurnTerminal::Final {
                content: "current: alpha".to_owned()
            }
        );
        let replay = handle
            .submit_turn(session, first_turn, "alpha", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(replay, first);
        assert!(
            handle
                .submit_turn(session, first_turn, "changed", Duration::from_secs(1))
                .await
                .is_err()
        );

        let second = handle
            .submit_turn(session, TurnId::new(), "beta", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            second.terminal,
            TurnTerminal::Final {
                content: "previous: alpha; current: beta".to_owned()
            }
        );

        let timeout_turn = TurnId::new();
        let timed_out = handle
            .submit_turn(session, timeout_turn, "timeout", Duration::from_millis(5))
            .await
            .unwrap();
        assert_eq!(timed_out.terminal, TurnTerminal::TimedOut);
        assert_eq!(
            handle
                .submit_turn(session, timeout_turn, "timeout", Duration::from_secs(1))
                .await
                .unwrap(),
            timed_out,
            "a late retry must observe the one stored terminal"
        );

        let cancelled_turn = TurnId::new();
        let submitting_handle = handle.clone();
        let submit = tokio::spawn(async move {
            submitting_handle
                .submit_turn(session, cancelled_turn, "cancel me", Duration::from_secs(1))
                .await
        });
        wait_for_active_turn(&handle, cancelled_turn).await;
        assert_eq!(
            handle.cancel(session, cancelled_turn).unwrap(),
            CancelResult::CancellationRequested
        );
        let cancelled = submit.await.unwrap().unwrap();
        assert_eq!(cancelled.terminal, TurnTerminal::Cancelled);
        assert_eq!(
            handle.cancel(session, cancelled_turn).unwrap(),
            CancelResult::AlreadyTerminal {
                terminal: TurnTerminal::Cancelled
            }
        );

        let recovered = handle
            .submit_turn(session, TurnId::new(), "gamma", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            recovered.terminal,
            TurnTerminal::Final {
                content: "previous: beta; current: gamma".to_owned()
            }
        );

        let other_active_session = SessionId::new();
        let other_active_turn = TurnId::new();
        let other_active_handle = handle.clone();
        let other_active_submit = tokio::spawn(async move {
            other_active_handle
                .submit_turn(
                    other_active_session,
                    other_active_turn,
                    "other active",
                    Duration::from_secs(1),
                )
                .await
        });
        wait_for_active_turn(&handle, other_active_turn).await;
        let queued_cancel_session = SessionId::new();
        let queued_cancel_turn = TurnId::new();
        assert_eq!(
            handle
                .cancel(queued_cancel_session, queued_cancel_turn)
                .unwrap(),
            CancelResult::CancellationRequested,
            "a different active turn must not erase a pre-cancel tombstone"
        );
        assert!(matches!(
            other_active_submit.await.unwrap().unwrap().terminal,
            TurnTerminal::Final { .. }
        ));
        assert_eq!(
            handle
                .submit_turn(
                    queued_cancel_session,
                    queued_cancel_turn,
                    "queued cancel",
                    Duration::from_secs(1),
                )
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Cancelled
        );

        let cancelled_before_submit_session = SessionId::new();
        let cancelled_before_submit_turn = TurnId::new();
        assert_eq!(
            handle
                .cancel(
                    cancelled_before_submit_session,
                    cancelled_before_submit_turn
                )
                .unwrap(),
            CancelResult::CancellationRequested
        );
        assert_eq!(
            handle
                .submit_turn(
                    cancelled_before_submit_session,
                    cancelled_before_submit_turn,
                    "must not run",
                    Duration::from_secs(1),
                )
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Cancelled
        );

        let aborted_turn = TurnId::new();
        let aborted_handle = handle.clone();
        let aborted_submit = tokio::spawn(async move {
            aborted_handle
                .submit_turn(session, aborted_turn, "abort me", Duration::from_secs(1))
                .await
        });
        wait_for_active_turn(&handle, aborted_turn).await;
        aborted_submit.abort();
        assert!(aborted_submit.await.is_err());
        assert_eq!(
            handle.cancel(session, aborted_turn).unwrap(),
            CancelResult::AlreadyTerminal {
                terminal: TurnTerminal::Cancelled
            },
            "dropping the submit future must seal one cancellation terminal"
        );

        let ledger_session = SessionId::new();
        let mut first_ledger_turn = None;
        for index in 0..MAX_RETAINED_TURNS_PER_SESSION {
            let turn = TurnId::new();
            let input = format!("ledger-{index}");
            assert_eq!(
                handle.cancel(ledger_session, turn).unwrap(),
                CancelResult::CancellationRequested
            );
            assert_eq!(
                handle
                    .submit_turn(ledger_session, turn, &input, Duration::from_secs(1))
                    .await
                    .unwrap()
                    .terminal,
                TurnTerminal::Cancelled
            );
            if index == 0 {
                first_ledger_turn = Some((turn, input));
            }
        }
        assert!(
            handle.cancel(ledger_session, TurnId::new()).is_err(),
            "a full terminal ledger must reject a new Turn instead of forgetting an old one"
        );
        let (first_ledger_turn, first_ledger_input) = first_ledger_turn.unwrap();
        assert_eq!(
            handle
                .submit_turn(
                    ledger_session,
                    first_ledger_turn,
                    &first_ledger_input,
                    Duration::from_secs(1),
                )
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Cancelled
        );
        assert!(
            handle
                .submit_turn(
                    ledger_session,
                    first_ledger_turn,
                    "conflict",
                    Duration::from_secs(1),
                )
                .await
                .is_err()
        );

        let mut last_cancelled = None;
        for _ in 0..=MAX_SESSIONS {
            let bounded_session = SessionId::new();
            let bounded_turn = TurnId::new();
            assert_eq!(
                handle.cancel(bounded_session, bounded_turn).unwrap(),
                CancelResult::CancellationRequested
            );
            last_cancelled = Some((bounded_session, bounded_turn));
        }
        let (bounded_session, bounded_turn) = last_cancelled.unwrap();
        assert_eq!(
            handle
                .submit_turn(
                    bounded_session,
                    bounded_turn,
                    "bounded",
                    Duration::from_secs(1),
                )
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Cancelled,
            "new ephemeral sessions must evict old inactive sessions instead of failing forever"
        );

        tokio::time::timeout(Duration::from_secs(1), runtime.stop())
            .await
            .expect("Agent shutdown must be wall-clock bounded")
            .unwrap();
        assert!(
            handle
                .submit_turn(session, TurnId::new(), "after stop", Duration::from_secs(1),)
                .await
                .is_err()
        );
    }

    fn runtime_identity(label: &str) -> RuntimeHostIdentity {
        RuntimeHostIdentity::new(RuntimeHostId::new(label).unwrap())
    }

    async fn wait_for_active_turn(handle: &AgentHandle, turn_id: TurnId) {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let inner = handle.upgrade().unwrap();
            let is_active = lock_state(&inner)
                .active_turn
                .as_ref()
                .is_some_and(|active| active.turn_id == turn_id);
            if is_active {
                return;
            }
            assert!(Instant::now() < deadline, "turn did not become active");
            tokio::task::yield_now().await;
        }
    }
}
