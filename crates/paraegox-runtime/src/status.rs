use std::fmt::{self, Display, Formatter};
use std::sync::{Arc, RwLock};

use paraegox_deck::{CardDefinitionRef, CardKey, DeckKey};
use paraegox_kernel::{RuntimeHostEpoch, RuntimeHostId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
            pub(crate) fn new() -> Self {
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
    pub(crate) snapshot: Arc<RwLock<RuntimeHostSnapshot>>,
}

impl RuntimeStatusReader {
    pub fn snapshot(&self) -> RuntimeHostSnapshot {
        self.snapshot
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(crate) fn set_runtime_host_state(&self, state: RuntimeHostState) {
        self.snapshot
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .state = state;
    }

    pub(crate) fn set_deck_run_state(&self, state: DeckRunState) {
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

    pub(crate) fn set_card_state(&self, index: usize, state: CardInstanceState) {
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
