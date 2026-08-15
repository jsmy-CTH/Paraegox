//! Agent CoreService, built-in Agent Card, and typed conversation client.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use paraegox_deck::CardDefinitionRef;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod provider;
mod service;
mod transport;

pub use provider::DeepSeekV4FlashConfig;
pub use service::{AgentCard, AgentHandle, AgentService};
pub use transport::{AgentConversationClient, agent_query_binding};

pub const BUILTIN_AGENT_DEFINITION: &str = "builtin.agent.deterministic@1";
pub const DEEPSEEK_V4_FLASH_AGENT_DEFINITION: &str = "builtin.agent.deepseek-v4-flash@1";

const MAX_INPUT_BYTES: usize = 4 * 1024;
const MAX_AGENT_WIRE_RESPONSE_BYTES: usize = 60 * 1024;
const MAX_TURN_DEADLINE: Duration = Duration::from_secs(60);

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

#[derive(Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub(crate) enum WireResponse {
    Turn {
        result: TurnResult,
    },
    Cancel {
        session_id: SessionId,
        turn_id: TurnId,
        result: CancelResult,
    },
}

pub(crate) fn validate_input(input: &str) -> Result<(), AgentError> {
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

pub(crate) fn validate_turn_deadline(deadline: Duration) -> Result<(), AgentError> {
    if deadline.is_zero() || deadline > MAX_TURN_DEADLINE {
        return Err(AgentError::new(
            "Agent turn deadline must be greater than zero and at most 60 seconds",
        ));
    }
    Ok(())
}

pub(crate) fn wire_safe_terminal(
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

#[derive(Debug)]
pub struct AgentError {
    message: String,
}

impl AgentError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub(crate) fn context(context: &str, error: impl Display) -> Self {
        Self::new(format!("{context}: {error}"))
    }
}

impl Display for AgentError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AgentError {}
