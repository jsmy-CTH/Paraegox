use std::error::Error;
use std::fmt::{self, Display, Formatter};

use async_trait::async_trait;
use paraegox_deck::{CardDefinitionRef, CardKey, DeckLock};

use crate::{BoxError, CardInstanceId};

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
    pub(crate) lock: DeckLock,
    pub(crate) implementations: Vec<Box<dyn CardImplementation>>,
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

pub(crate) struct DeckOwner {
    pub(crate) lock: DeckLock,
    pub(crate) implementations: Vec<Box<dyn CardImplementation>>,
    pub(crate) card_instance_ids: Vec<CardInstanceId>,
}

impl DeckOwner {
    pub(crate) fn context(&self, index: usize) -> CardContext {
        let card = &self.lock.cards()[index];
        CardContext {
            card_instance_id: self.card_instance_ids[index],
            card_key: card.key().clone(),
            definition: card.definition().clone(),
        }
    }
}
