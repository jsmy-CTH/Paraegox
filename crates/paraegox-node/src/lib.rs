//! A Node is the addressable resource and failure boundary that owns one RuntimeHost.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use paraegox_fabric::{FabricError, FabricService, query_one};
use paraegox_kernel::{NodeId, NodeIncarnation};
use paraegox_runtime::{
    RuntimeHost, RuntimeHostError, RuntimeHostIdentity, RuntimeHostSnapshot, RuntimeHostState,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeIdentity {
    pub node_id: NodeId,
    pub incarnation: NodeIncarnation,
}

impl NodeIdentity {
    pub fn new(node_id: NodeId) -> Self {
        Self {
            node_id,
            incarnation: NodeIncarnation::new(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FabricServiceSnapshot {
    pub ready: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeRuntimeStatus {
    pub node: NodeIdentity,
    pub runtime: RuntimeHostSnapshot,
    pub fabric: FabricServiceSnapshot,
}

impl NodeRuntimeStatus {
    pub fn ready(
        node: NodeIdentity,
        runtime: RuntimeHostSnapshot,
    ) -> Result<Self, NodeStatusError> {
        if runtime.state != RuntimeHostState::Ready {
            return Err(NodeStatusError::new("RuntimeHost is not ready"));
        }
        Ok(Self {
            node,
            runtime,
            fabric: FabricServiceSnapshot { ready: true },
        })
    }
}

pub struct Node {
    identity: NodeIdentity,
    runtime: RuntimeHost<FabricService>,
}

impl Node {
    pub fn new(
        identity: NodeIdentity,
        runtime_identity: RuntimeHostIdentity,
        listen_endpoint: impl Into<String>,
    ) -> Result<Self, FabricError> {
        let binding_node = identity.clone();
        let fabric = FabricService::new(
            listen_endpoint,
            runtime_status_key(&identity.node_id),
            move |runtime| {
                encode_runtime_status(&binding_node, runtime).map_err(|error| error.to_string())
            },
        )?;
        Ok(Self {
            identity,
            runtime: RuntimeHost::new(runtime_identity, fabric),
        })
    }

    pub fn status(&self) -> Result<NodeRuntimeStatus, NodeStatusError> {
        NodeRuntimeStatus::ready(self.identity.clone(), self.runtime.snapshot())
    }

    pub async fn start(&mut self) -> Result<(), RuntimeHostError> {
        self.runtime.start().await
    }

    pub async fn stop(&mut self) -> Result<(), RuntimeHostError> {
        self.runtime.stop().await
    }
}

pub async fn probe_node(
    target: &NodeId,
    connect_endpoint: &str,
    deadline: Duration,
) -> Result<NodeRuntimeStatus, NodeStatusError> {
    let payload = query_one(connect_endpoint, &runtime_status_key(target), deadline)
        .await
        .map_err(|error| NodeStatusError::new(error.to_string()))?;
    decode_runtime_status(target, &payload)
}

fn runtime_status_key(node_id: &NodeId) -> String {
    format!("paraegox/v1/nodes/{}/runtime/status", node_id.as_str())
}

fn encode_runtime_status(
    node: &NodeIdentity,
    runtime: RuntimeHostSnapshot,
) -> Result<Vec<u8>, NodeStatusError> {
    let status = NodeRuntimeStatus::ready(node.clone(), runtime)?;
    serde_json::to_vec(&status)
        .map_err(|error| NodeStatusError::new(format!("could not encode Node status: {error}")))
}

fn decode_runtime_status(
    target: &NodeId,
    payload: &[u8],
) -> Result<NodeRuntimeStatus, NodeStatusError> {
    let status: NodeRuntimeStatus = serde_json::from_slice(payload)
        .map_err(|error| NodeStatusError::new(format!("invalid Node status JSON: {error}")))?;
    if &status.node.node_id != target {
        return Err(NodeStatusError::new(format!(
            "Node identity mismatch: requested {target}, received {}",
            status.node.node_id
        )));
    }
    if status.runtime.state != RuntimeHostState::Ready || !status.fabric.ready {
        return Err(NodeStatusError::new(
            "Node, RuntimeHost, or FabricService is not ready",
        ));
    }
    Ok(status)
}

#[derive(Debug)]
pub struct NodeStatusError {
    message: String,
}

impl NodeStatusError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl Display for NodeStatusError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for NodeStatusError {}
