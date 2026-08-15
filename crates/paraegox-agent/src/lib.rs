//! Agent CoreService, built-in Agent Card, and typed conversation client.

use std::collections::{HashMap, VecDeque};
use std::env;
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
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::{Client, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::{Notify, watch};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

pub const BUILTIN_AGENT_DEFINITION: &str = "builtin.agent.deterministic@1";
pub const DEEPSEEK_V4_FLASH_AGENT_DEFINITION: &str = "builtin.agent.deepseek-v4-flash@1";

const MAX_SESSIONS: usize = 64;
const MAX_RETAINED_TURNS_PER_SESSION: usize = 32;
const MAX_INPUT_BYTES: usize = 4 * 1024;
const MAX_MODEL_CONTENT_BYTES: usize = 16 * 1024;
const MAX_MODEL_CONTEXT_BYTES: usize = 256 * 1024;
const MAX_AGENT_WIRE_RESPONSE_BYTES: usize = 60 * 1024;
const MAX_PROVIDER_REQUEST_BYTES: usize = 512 * 1024;
const MAX_PROVIDER_RESPONSE_BYTES: usize = 64 * 1024;
const DEEPSEEK_MAX_TOKENS: u16 = 512;
const DEEPSEEK_V4_FLASH_MODEL: &str = "deepseek-v4-flash";
const DEEPSEEK_CHAT_COMPLETIONS_URL: &str = "https://api.deepseek.com/chat/completions";
const BUILTIN_AGENT_SYSTEM_PROMPT: &str =
    "You are Paraegox, a concise embodied-intelligence agent. Answer the user directly.";
const MAX_TURN_DEADLINE: Duration = Duration::from_secs(60);
const CLIENT_REPLY_GRACE: Duration = Duration::from_millis(250);

pub fn builtin_agent_definition() -> CardDefinitionRef {
    CardDefinitionRef::new(BUILTIN_AGENT_DEFINITION)
}

pub fn deepseek_v4_flash_agent_definition() -> CardDefinitionRef {
    CardDefinitionRef::new(DEEPSEEK_V4_FLASH_AGENT_DEFINITION)
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

pub struct DeepSeekV4FlashConfig {
    api_key: SecretString,
}

impl DeepSeekV4FlashConfig {
    pub fn from_env() -> Result<Self, AgentError> {
        let api_key = env::var("DEEPSEEK_API_KEY")
            .map_err(|_| AgentError::new("DEEPSEEK_API_KEY is not set"))?;
        validate_api_key(&api_key)?;
        Ok(Self {
            api_key: SecretString::from(api_key),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnFailure {
    ProviderRejected,
    ProviderUnavailable,
    InvalidProviderResponse,
    ContextLimit,
}

impl Display for TurnFailure {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ProviderRejected => "model provider rejected the request",
            Self::ProviderUnavailable => "model provider is unavailable",
            Self::InvalidProviderResponse => "model provider returned an invalid response",
            Self::ContextLimit => "Agent session context limit was reached",
        })
    }
}

impl Error for TurnFailure {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "terminal", rename_all = "snake_case")]
pub enum TurnTerminal {
    Final { content: String },
    Cancelled,
    TimedOut,
    Failed { reason: TurnFailure },
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
    system_prompt: &'static str,
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
    provider: CompletionProvider,
}

struct AgentState {
    lifecycle: ServiceLifecycle,
    active_profile: Option<ActiveProfile>,
    sessions: HashMap<SessionId, SessionState>,
    session_order: VecDeque<SessionId>,
    active_turn: Option<ActiveTurn>,
}

struct ActiveProfile {
    card_instance_id: CardInstanceId,
    system_prompt: &'static str,
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

enum CompletionProvider {
    Deterministic { response_delay: Duration },
    DeepSeekV4Flash(DeepSeekV4FlashProvider),
}

struct DeepSeekV4FlashProvider {
    client: Client,
    api_key: SecretString,
    endpoint: String,
}

#[derive(Clone, Serialize)]
struct ModelMessage {
    role: ModelRole,
    content: String,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum ModelRole {
    System,
    User,
    Assistant,
}

struct ModelContext {
    messages: Vec<ModelMessage>,
}

#[derive(Serialize)]
struct DeepSeekChatRequest<'a> {
    model: &'static str,
    messages: &'a [ModelMessage],
    thinking: DeepSeekThinking,
    stream: bool,
    max_tokens: u16,
}

#[derive(Serialize)]
struct DeepSeekThinking {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Deserialize)]
struct DeepSeekChatResponse {
    choices: Vec<DeepSeekChoice>,
}

#[derive(Deserialize)]
struct DeepSeekChoice {
    message: DeepSeekResponseMessage,
    finish_reason: String,
}

#[derive(Deserialize)]
struct DeepSeekResponseMessage {
    content: Option<String>,
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

    pub fn with_deepseek_v4_flash(config: DeepSeekV4FlashConfig) -> Result<Self, AgentError> {
        let provider = DeepSeekV4FlashProvider::new(config)?;
        Ok(Self::with_provider(CompletionProvider::DeepSeekV4Flash(
            provider,
        )))
    }

    fn with_response_delay(response_delay: Duration) -> Self {
        Self::with_provider(CompletionProvider::Deterministic { response_delay })
    }

    fn with_provider(provider: CompletionProvider) -> Self {
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
                provider,
            }),
        }
    }

    #[cfg(test)]
    fn with_deepseek_v4_flash_endpoint(
        config: DeepSeekV4FlashConfig,
        endpoint: String,
    ) -> Result<Self, AgentError> {
        let provider = DeepSeekV4FlashProvider::for_test(config, endpoint)?;
        Ok(Self::with_provider(CompletionProvider::DeepSeekV4Flash(
            provider,
        )))
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

impl CompletionProvider {
    fn card_definition(&self) -> &'static str {
        match self {
            Self::Deterministic { .. } => BUILTIN_AGENT_DEFINITION,
            Self::DeepSeekV4Flash(_) => DEEPSEEK_V4_FLASH_AGENT_DEFINITION,
        }
    }

    async fn complete(&self, context: &ModelContext) -> Result<String, TurnFailure> {
        match self {
            Self::Deterministic { response_delay } => {
                sleep(*response_delay).await;
                Ok(deterministic_final(context))
            }
            Self::DeepSeekV4Flash(provider) => provider.complete(context).await,
        }
    }
}

impl DeepSeekV4FlashProvider {
    fn new(config: DeepSeekV4FlashConfig) -> Result<Self, AgentError> {
        Self::build(config, DEEPSEEK_CHAT_COMPLETIONS_URL.to_owned(), true)
    }

    #[cfg(test)]
    fn for_test(config: DeepSeekV4FlashConfig, endpoint: String) -> Result<Self, AgentError> {
        Self::build(config, endpoint, false)
    }

    fn build(
        config: DeepSeekV4FlashConfig,
        endpoint: String,
        https_only: bool,
    ) -> Result<Self, AgentError> {
        let client = Client::builder()
            .https_only(https_only)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|error| AgentError::context("could not construct DeepSeek client", error))?;
        Ok(Self {
            client,
            api_key: config.api_key,
            endpoint,
        })
    }

    async fn complete(&self, context: &ModelContext) -> Result<String, TurnFailure> {
        let body = encode_deepseek_request(context)?;
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {}", self.api_key.expose_secret()))
                .map_err(|_| TurnFailure::ProviderRejected)?;
        authorization.set_sensitive(true);

        let mut response = self
            .client
            .post(&self.endpoint)
            .header(AUTHORIZATION, authorization)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| TurnFailure::ProviderUnavailable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(classify_provider_status(status));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_PROVIDER_RESPONSE_BYTES as u64)
        {
            return Err(TurnFailure::InvalidProviderResponse);
        }
        let mut payload = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| TurnFailure::ProviderUnavailable)?
        {
            let remaining = MAX_PROVIDER_RESPONSE_BYTES
                .checked_sub(payload.len())
                .ok_or(TurnFailure::InvalidProviderResponse)?;
            if chunk.len() > remaining {
                return Err(TurnFailure::InvalidProviderResponse);
            }
            payload.extend_from_slice(&chunk);
        }
        decode_deepseek_response(&payload)
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

        let (context, mut cancellation) = {
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

            let system_prompt = state
                .active_profile
                .as_ref()
                .expect("turn admission requires an active profile")
                .system_prompt;
            let context = match build_model_context(
                system_prompt,
                state
                    .sessions
                    .get(&session_id)
                    .expect("turn admission owns a bounded session"),
                input,
            ) {
                Ok(context) => context,
                Err(reason) => {
                    let terminal = TurnTerminal::Failed { reason };
                    record_completed_turn(
                        state
                            .sessions
                            .get_mut(&session_id)
                            .expect("turn admission owns a bounded session"),
                        turn_id,
                        input,
                        terminal.clone(),
                    );
                    return Ok(TurnResult {
                        session_id,
                        turn_id,
                        terminal,
                    });
                }
            };
            let (cancellation_sender, cancellation_receiver) = watch::channel(false);
            state.active_turn = Some(ActiveTurn {
                session_id,
                turn_id,
                cancellation: cancellation_sender,
            });
            (context, cancellation_receiver)
        };
        let guard = ActiveTurnGuard::new(Arc::clone(&inner), session_id, turn_id, input);

        let terminal = tokio::select! {
            () = wait_for_cancellation(&mut cancellation) => TurnTerminal::Cancelled,
            result = timeout(deadline, inner.provider.complete(&context)) => match result {
                Ok(Ok(content)) => TurnTerminal::Final { content },
                Ok(Err(reason)) => TurnTerminal::Failed { reason },
                Err(_) => TurnTerminal::TimedOut,
            },
        };

        let terminal = guard.complete(wire_safe_terminal(session_id, turn_id, terminal))?;

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

    fn activate_profile(
        &self,
        card_instance_id: CardInstanceId,
        definition: &CardDefinitionRef,
        system_prompt: &'static str,
    ) -> Result<(), AgentError> {
        let inner = self.upgrade()?;
        if inner.provider.card_definition() != definition.as_str() {
            return Err(AgentError::new(
                "Agent Card definition does not match the configured provider",
            ));
        }
        let mut state = lock_state(&inner);
        if state.lifecycle != ServiceLifecycle::Running {
            return Err(AgentError::new("AgentService is not running"));
        }
        if state.active_profile.is_some() {
            return Err(AgentError::new("an Agent profile is already active"));
        }
        state.active_profile = Some(ActiveProfile {
            card_instance_id,
            system_prompt,
        });
        Ok(())
    }

    async fn deactivate_profile(&self, card_instance_id: CardInstanceId) -> Result<(), AgentError> {
        let inner = self.upgrade()?;
        {
            let mut state = lock_state(&inner);
            match state.active_profile.as_ref() {
                Some(active) if active.card_instance_id == card_instance_id => {
                    state.active_profile = None;
                }
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
            system_prompt: BUILTIN_AGENT_SYSTEM_PROMPT,
            handle,
            active_instance: None,
        }
    }

    pub fn new_deepseek_v4_flash(key: CardKey, handle: AgentHandle) -> Self {
        Self {
            key,
            definition: deepseek_v4_flash_agent_definition(),
            system_prompt: BUILTIN_AGENT_SYSTEM_PROMPT,
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
            .activate_profile(
                context.card_instance_id,
                &self.definition,
                self.system_prompt,
            )
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

fn validate_api_key(api_key: &str) -> Result<(), AgentError> {
    if api_key.trim().is_empty() {
        return Err(AgentError::new("DEEPSEEK_API_KEY must not be empty"));
    }
    if api_key.len() > 4 * 1024 || !api_key.is_ascii() || api_key.chars().any(char::is_control) {
        return Err(AgentError::new("DEEPSEEK_API_KEY is invalid"));
    }
    Ok(())
}

fn build_model_context(
    system_prompt: &'static str,
    session: &SessionState,
    current_input: &str,
) -> Result<ModelContext, TurnFailure> {
    let mut messages = Vec::with_capacity(session.turns.len() * 2 + 2);
    let mut context_bytes = 0usize;
    push_context_message(
        &mut messages,
        &mut context_bytes,
        ModelRole::System,
        system_prompt,
    )?;
    for turn in &session.turns {
        if let TurnTerminal::Final { content } = &turn.terminal {
            push_context_message(
                &mut messages,
                &mut context_bytes,
                ModelRole::User,
                &turn.input,
            )?;
            push_context_message(
                &mut messages,
                &mut context_bytes,
                ModelRole::Assistant,
                content,
            )?;
        }
    }
    push_context_message(
        &mut messages,
        &mut context_bytes,
        ModelRole::User,
        current_input,
    )?;
    Ok(ModelContext { messages })
}

fn push_context_message(
    messages: &mut Vec<ModelMessage>,
    context_bytes: &mut usize,
    role: ModelRole,
    content: &str,
) -> Result<(), TurnFailure> {
    *context_bytes = context_bytes
        .checked_add(content.len())
        .filter(|total| *total <= MAX_MODEL_CONTEXT_BYTES)
        .ok_or(TurnFailure::ContextLimit)?;
    messages.push(ModelMessage {
        role,
        content: content.to_owned(),
    });
    Ok(())
}

fn classify_provider_status(status: StatusCode) -> TurnFailure {
    if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
        TurnFailure::ProviderUnavailable
    } else {
        TurnFailure::ProviderRejected
    }
}

fn encode_deepseek_request(context: &ModelContext) -> Result<Vec<u8>, TurnFailure> {
    let request = DeepSeekChatRequest {
        model: DEEPSEEK_V4_FLASH_MODEL,
        messages: &context.messages,
        thinking: DeepSeekThinking { kind: "disabled" },
        stream: false,
        max_tokens: DEEPSEEK_MAX_TOKENS,
    };
    let body = serde_json::to_vec(&request).map_err(|_| TurnFailure::InvalidProviderResponse)?;
    if body.len() > MAX_PROVIDER_REQUEST_BYTES {
        return Err(TurnFailure::ContextLimit);
    }
    Ok(body)
}

fn decode_deepseek_response(payload: &[u8]) -> Result<String, TurnFailure> {
    let response: DeepSeekChatResponse =
        serde_json::from_slice(payload).map_err(|_| TurnFailure::InvalidProviderResponse)?;
    let [choice] = response.choices.as_slice() else {
        return Err(TurnFailure::InvalidProviderResponse);
    };
    if choice.finish_reason != "stop" {
        return Err(TurnFailure::InvalidProviderResponse);
    }
    let content = choice
        .message
        .content
        .as_deref()
        .ok_or(TurnFailure::InvalidProviderResponse)?;
    if !is_safe_model_content(content) {
        return Err(TurnFailure::InvalidProviderResponse);
    }
    Ok(content.to_owned())
}

fn is_safe_model_content(content: &str) -> bool {
    !content.trim().is_empty()
        && content.len() <= MAX_MODEL_CONTENT_BYTES
        && !content
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
}

fn wire_safe_terminal(
    session_id: SessionId,
    turn_id: TurnId,
    terminal: TurnTerminal,
) -> TurnTerminal {
    let response = WireResponse::Turn {
        result: TurnResult {
            session_id,
            turn_id,
            terminal: terminal.clone(),
        },
    };
    match serde_json::to_vec(&response) {
        Ok(encoded) if encoded.len() <= MAX_AGENT_WIRE_RESPONSE_BYTES => terminal,
        _ => TurnTerminal::Failed {
            reason: TurnFailure::InvalidProviderResponse,
        },
    }
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

fn deterministic_final(context: &ModelContext) -> String {
    let mut user_inputs = context
        .messages
        .iter()
        .rev()
        .filter(|message| matches!(message.role, ModelRole::User))
        .map(|message| message.content.as_str());
    let current = user_inputs
        .next()
        .expect("an admitted model context always contains the current user input");
    match user_inputs.next() {
        Some(previous) => format!("previous: {previous}; current: {current}"),
        None => format!("current: {current}"),
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
    use std::io::ErrorKind;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc as TestArc, Mutex as TestMutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use paraegox_deck::{Card, CardKey, DeckCompiler, DeckKey, DeckSpec};
    use paraegox_kernel::RuntimeHostId;
    use paraegox_runtime::{DeckLaunch, RuntimeHost, RuntimeHostIdentity};
    use serde_json::Value;

    use super::*;

    struct CapturedRequest {
        body: Value,
        target: String,
        authorized: bool,
    }

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

    #[tokio::test]
    async fn deepseek_contract_is_bounded_safe_and_preserves_only_successful_history() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}/chat/completions", listener.local_addr().unwrap());
        let api_key = Uuid::new_v4().to_string();
        let expected_authorization = format!("Bearer {api_key}");
        let requests = TestArc::new(TestMutex::new(Vec::new()));
        let captured_requests = TestArc::clone(&requests);
        let server = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut index = 0usize;
            while index < 7 {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        assert!(
                            Instant::now() < deadline,
                            "fake Provider did not receive all expected requests"
                        );
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => panic!("fake Provider accept failed: {error}"),
                };
                stream.set_nonblocking(false).unwrap();
                let request = read_json_request(&mut stream, &expected_authorization);
                captured_requests.lock().unwrap().push(request);
                let (delay, status, response) = match index {
                    0 => (
                        Duration::ZERO,
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"assistant one"},"finish_reason":"stop"}]}"#,
                    ),
                    1 => (
                        Duration::ZERO,
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"assistant two"},"finish_reason":"stop"}]}"#,
                    ),
                    2 => (
                        Duration::ZERO,
                        "503 Service Unavailable",
                        r#"{"error":"unavailable"}"#,
                    ),
                    3 => (
                        Duration::from_secs(1),
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"late timeout"},"finish_reason":"stop"}]}"#,
                    ),
                    4 => (
                        Duration::ZERO,
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"\u001b]0;unsafe\u0007"},"finish_reason":"stop"}]}"#,
                    ),
                    5 => (
                        Duration::from_secs(1),
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"late cancel"},"finish_reason":"stop"}]}"#,
                    ),
                    _ => (
                        Duration::ZERO,
                        "200 OK",
                        r#"{"choices":[{"message":{"content":"assistant after failures"},"finish_reason":"stop"}]}"#,
                    ),
                };
                thread::sleep(delay);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    response.len()
                );
                let _ = stream.flush();
                index += 1;
            }
        });

        let config = DeepSeekV4FlashConfig {
            api_key: SecretString::from(api_key),
        };
        let service = AgentService::with_deepseek_v4_flash_endpoint(config, endpoint).unwrap();
        let handle = service.handle();
        let key = CardKey::new("agent");
        let definition = deepseek_v4_flash_agent_definition();
        let compiler = DeckCompiler::new([definition.clone()]).unwrap();
        let lock = compiler
            .compile(&DeckSpec {
                key: DeckKey::new("deepseek-agent-test"),
                cards: vec![Card {
                    key: key.clone(),
                    definition,
                }],
            })
            .unwrap();
        let launch = DeckLaunch::new(
            lock,
            vec![Box::new(AgentCard::new_deepseek_v4_flash(
                key,
                handle.clone(),
            ))],
        )
        .unwrap();
        let mut runtime = RuntimeHost::with_deck(
            runtime_identity("deepseek-agent-runtime"),
            vec![Box::new(service)],
            launch,
        )
        .unwrap();
        runtime.start().await.unwrap();

        let session = SessionId::new();
        assert_eq!(
            handle
                .submit_turn(session, TurnId::new(), "user one", Duration::from_secs(1))
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Final {
                content: "assistant one".to_owned()
            }
        );
        assert_eq!(
            handle
                .submit_turn(session, TurnId::new(), "user two", Duration::from_secs(1))
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Final {
                content: "assistant two".to_owned()
            }
        );
        let failed_turn = TurnId::new();
        let failed = handle
            .submit_turn(session, failed_turn, "user three", Duration::from_secs(1))
            .await
            .unwrap();
        assert_eq!(
            failed.terminal,
            TurnTerminal::Failed {
                reason: TurnFailure::ProviderUnavailable
            }
        );
        assert_eq!(
            handle
                .submit_turn(session, failed_turn, "user three", Duration::from_secs(1),)
                .await
                .unwrap(),
            failed,
            "a failed terminal must replay without another provider request"
        );

        assert_eq!(
            requests.lock().unwrap().len(),
            3,
            "replaying a failed Turn must not send another HTTP request"
        );

        let timed_out = handle
            .submit_turn(
                session,
                TurnId::new(),
                "must time out",
                Duration::from_millis(500),
            )
            .await
            .unwrap();
        assert_eq!(timed_out.terminal, TurnTerminal::TimedOut);

        let unsafe_output = handle
            .submit_turn(
                session,
                TurnId::new(),
                "reject unsafe output",
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        assert_eq!(
            unsafe_output.terminal,
            TurnTerminal::Failed {
                reason: TurnFailure::InvalidProviderResponse
            }
        );

        let cancelled_turn = TurnId::new();
        let submitting_handle = handle.clone();
        let cancelled_submit = tokio::spawn(async move {
            submitting_handle
                .submit_turn(
                    session,
                    cancelled_turn,
                    "must cancel",
                    Duration::from_secs(3),
                )
                .await
        });
        wait_for_request_count(&requests, 6).await;
        assert_eq!(
            handle.cancel(session, cancelled_turn).unwrap(),
            CancelResult::CancellationRequested
        );
        assert_eq!(
            cancelled_submit.await.unwrap().unwrap().terminal,
            TurnTerminal::Cancelled
        );

        assert_eq!(
            handle
                .submit_turn(
                    session,
                    TurnId::new(),
                    "after failures",
                    Duration::from_secs(3),
                )
                .await
                .unwrap()
                .terminal,
            TurnTerminal::Final {
                content: "assistant after failures".to_owned()
            }
        );

        let escape_heavy = ModelContext {
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: "\\".repeat(MAX_MODEL_CONTEXT_BYTES),
            }],
        };
        assert_eq!(
            encode_deepseek_request(&escape_heavy).unwrap_err(),
            TurnFailure::ContextLimit,
            "the encoded HTTP request has its own bound"
        );
        assert!(matches!(
            wire_safe_terminal(
                SessionId::new(),
                TurnId::new(),
                TurnTerminal::Final {
                    content: "\"".repeat(MAX_MODEL_CONTENT_BYTES),
                },
            ),
            TurnTerminal::Final { .. }
        ));

        runtime.stop().await.unwrap();
        server.join().unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 7, "each new Turn makes exactly one request");
        assert!(requests.iter().all(|request| request.authorized));
        assert!(
            requests
                .iter()
                .all(|request| request.target == "/chat/completions")
        );
        let request = &requests[2].body;
        assert_eq!(request["model"], DEEPSEEK_V4_FLASH_MODEL);
        assert_eq!(request["thinking"]["type"], "disabled");
        assert_eq!(request["stream"], false);
        assert_eq!(request["max_tokens"], DEEPSEEK_MAX_TOKENS);
        assert!(request.get("tools").is_none());
        assert_eq!(
            request["messages"],
            serde_json::json!([
                {"role": "system", "content": BUILTIN_AGENT_SYSTEM_PROMPT},
                {"role": "user", "content": "user one"},
                {"role": "assistant", "content": "assistant one"},
                {"role": "user", "content": "user two"},
                {"role": "assistant", "content": "assistant two"},
                {"role": "user", "content": "user three"}
            ])
        );
        assert_eq!(
            requests[6].body["messages"],
            serde_json::json!([
                {"role": "system", "content": BUILTIN_AGENT_SYSTEM_PROMPT},
                {"role": "user", "content": "user one"},
                {"role": "assistant", "content": "assistant one"},
                {"role": "user", "content": "user two"},
                {"role": "assistant", "content": "assistant two"},
                {"role": "user", "content": "after failures"}
            ]),
            "failed, timed-out, unsafe, and cancelled Turns must not enter history"
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

    async fn wait_for_request_count(
        requests: &TestArc<TestMutex<Vec<CapturedRequest>>>,
        expected: usize,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if requests.lock().unwrap().len() >= expected {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("fake Provider request count did not advance");
    }

    fn read_json_request(stream: &mut TcpStream, expected_authorization: &str) -> CapturedRequest {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut received = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 1024];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "HTTP request ended before its headers");
            received.extend_from_slice(&chunk[..read]);
            if let Some(position) = received.windows(4).position(|part| part == b"\r\n\r\n") {
                break position + 4;
            }
            assert!(
                received.len() <= 16 * 1024,
                "HTTP request headers exceeded the test bound"
            );
        };
        let headers = String::from_utf8(received[..header_end].to_vec()).unwrap();
        let mut lines = headers.lines();
        let target = lines
            .next()
            .and_then(|request_line| request_line.split_whitespace().nth(1))
            .expect("request should carry a target")
            .to_owned();
        let headers: Vec<_> = lines.collect();
        let content_length = headers
            .iter()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .expect("request should carry Content-Length");
        assert!(
            content_length <= MAX_PROVIDER_REQUEST_BYTES,
            "HTTP request body exceeded the test bound"
        );
        while received.len() < header_end + content_length {
            let mut chunk = [0u8; 4096];
            let read = stream.read(&mut chunk).unwrap();
            assert!(read > 0, "HTTP request ended before its body");
            received.extend_from_slice(&chunk[..read]);
        }
        let authorized = headers.iter().any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("authorization") && value.trim() == expected_authorization
            })
        });
        CapturedRequest {
            body: serde_json::from_slice(&received[header_end..header_end + content_length])
                .unwrap(),
            target,
            authorized,
        }
    }
}
