//! Typed Agent conversation transport over Fabric.

use std::time::Duration;

use paraegox_fabric::{FabricClient, FabricQueryBinding};
use paraegox_kernel::NodeId;
use serde::{Deserialize, Serialize};

use crate::service::AgentHandle;
use crate::{
    AgentError, CancelResult, SessionId, TurnId, TurnResult, WireResponse, validate_input,
    validate_turn_deadline,
};

const CLIENT_REPLY_GRACE: Duration = Duration::from_millis(250);

pub struct AgentConversationClient {
    fabric: FabricClient,
    binding_key: String,
    session_id: SessionId,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WireRequest {
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

pub(crate) async fn serve_wire_request(
    handle: &AgentHandle,
    payload: &[u8],
) -> Result<Vec<u8>, AgentError> {
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
