//! A Node is the addressable resource and failure boundary that owns one RuntimeHost.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use paraegox_agent::{AgentCard, AgentService, agent_query_binding, builtin_agent_definition};
use paraegox_deck::{Card, CardKey, DeckCompiler, DeckKey, DeckSpec};
use paraegox_fabric::{FabricError, FabricHandle, FabricQueryBinding, FabricService, query_one};
use paraegox_kernel::{NodeId, NodeIncarnation};
use paraegox_runtime::{
    CoreService, DeckLaunch, RuntimeHost, RuntimeHostError, RuntimeHostIdentity,
    RuntimeHostSnapshot, RuntimeHostState,
};
use serde::{Deserialize, Serialize};

const BUILTIN_AGENT_DECK_KEY: &str = "builtin-agent";
const BUILTIN_AGENT_CARD_KEY: &str = "agent";

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
    runtime: RuntimeHost,
    fabric_handle: FabricHandle,
}

impl Node {
    pub fn new(
        identity: NodeIdentity,
        runtime_identity: RuntimeHostIdentity,
        listen_endpoint: impl Into<String>,
        connect_endpoint: Option<String>,
    ) -> Result<Self, NodeBuildError> {
        let fabric = FabricService::new_with_bindings(
            listen_endpoint,
            connect_endpoint,
            vec![runtime_status_binding(identity.clone())?],
        )
        .map_err(|error| NodeBuildError::context("could not construct FabricService", error))?;
        let fabric_handle = fabric.handle();
        let core_services: Vec<Box<dyn CoreService>> = vec![Box::new(fabric)];
        let runtime = RuntimeHost::new(runtime_identity, core_services)
            .map_err(|error| NodeBuildError::context("could not construct RuntimeHost", error))?;
        Ok(Self {
            identity,
            runtime,
            fabric_handle,
        })
    }

    /// Constructs the smallest executable Deck: one built-in deterministic Agent Card.
    pub fn new_with_builtin_agent(
        identity: NodeIdentity,
        runtime_identity: RuntimeHostIdentity,
        listen_endpoint: impl Into<String>,
        connect_endpoint: Option<String>,
    ) -> Result<Self, NodeBuildError> {
        let agent_service = AgentService::new();
        let agent_handle = agent_service.handle();
        let bindings = vec![
            runtime_status_binding(identity.clone())?,
            agent_query_binding(&identity.node_id, agent_handle.clone()).map_err(|error| {
                NodeBuildError::context("could not construct Agent Fabric binding", error)
            })?,
        ];
        let fabric = FabricService::new_with_bindings(listen_endpoint, connect_endpoint, bindings)
            .map_err(|error| NodeBuildError::context("could not construct FabricService", error))?;
        let fabric_handle = fabric.handle();

        let definition = builtin_agent_definition();
        let card_key = CardKey::new(BUILTIN_AGENT_CARD_KEY);
        let deck = DeckSpec {
            key: DeckKey::new(BUILTIN_AGENT_DECK_KEY),
            cards: vec![Card {
                key: card_key.clone(),
                definition: definition.clone(),
            }],
        };
        let compiler = DeckCompiler::new([definition])
            .map_err(|error| NodeBuildError::context("invalid built-in Deck compiler", error))?;
        let lock = compiler
            .compile(&deck)
            .map_err(|error| NodeBuildError::context("could not compile built-in Deck", error))?;
        let card = AgentCard::new(card_key, agent_handle);
        let launch = DeckLaunch::new(lock, vec![Box::new(card)]).map_err(|error| {
            NodeBuildError::context("could not bind built-in Deck implementation", error)
        })?;
        let core_services: Vec<Box<dyn CoreService>> =
            vec![Box::new(fabric), Box::new(agent_service)];
        let runtime = RuntimeHost::with_deck(runtime_identity, core_services, launch)
            .map_err(|error| NodeBuildError::context("could not construct RuntimeHost", error))?;

        Ok(Self {
            identity,
            runtime,
            fabric_handle,
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

    pub async fn probe_peer(
        &self,
        target: &NodeId,
        deadline: Duration,
    ) -> Result<NodeRuntimeStatus, NodeStatusError> {
        if target == &self.identity.node_id {
            return Err(NodeStatusError::new("a Node cannot probe itself as a peer"));
        }
        let payload = self
            .fabric_handle
            .query_one(&runtime_status_key(target), deadline)
            .await
            .map_err(|error| NodeStatusError::new(error.to_string()))?;
        decode_runtime_status(target, &payload)
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

fn runtime_status_binding(node: NodeIdentity) -> Result<FabricQueryBinding, FabricError> {
    FabricQueryBinding::new(
        runtime_status_key(&node.node_id),
        move |runtime, _payload| {
            let result = encode_runtime_status(&node, runtime).map_err(|error| error.to_string());
            async move { result }
        },
    )
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

#[derive(Debug)]
pub struct NodeBuildError {
    message: String,
}

impl NodeBuildError {
    fn context(context: &str, error: impl Display) -> Self {
        Self {
            message: format!("{context}: {error}"),
        }
    }
}

impl From<FabricError> for NodeBuildError {
    fn from(error: FabricError) -> Self {
        Self::context("could not construct Fabric query binding", error)
    }
}

impl Display for NodeBuildError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for NodeBuildError {}
