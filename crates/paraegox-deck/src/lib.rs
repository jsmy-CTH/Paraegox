//! Pure Deck/Card workload models and deterministic Deck compilation.

use std::collections::BTreeSet;
use std::error::Error;
use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum number of cards accepted in one Deck specification.
pub const MAX_CARDS_PER_DECK: usize = 64;

const MAX_DECK_KEY_BYTES: usize = 128;
const MAX_CARD_KEY_BYTES: usize = 128;
const MAX_DEFINITION_REF_BYTES: usize = 256;
const DIGEST_DOMAIN: &[u8] = b"paraegox.deck-lock\0";

/// An exact, composition-provided Card definition identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CardDefinitionRef(pub String);

impl CardDefinitionRef {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CardDefinitionRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A Card's unique name inside one Deck.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CardKey(pub String);

impl CardKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for CardKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A Deck's composition identity.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeckKey(pub String);

impl DeckKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DeckKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// One named use of an exact Card definition in a Deck.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Card {
    pub key: CardKey,
    pub definition: CardDefinitionRef,
}

/// A declarative Deck workload before exact validation and resolution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeckSpec {
    pub key: DeckKey,
    pub cards: Vec<Card>,
}

/// A Card whose exact definition was accepted by the current composition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResolvedCard {
    key: CardKey,
    definition: CardDefinitionRef,
}

/// The deterministic, exact result consumed by a Deck runtime owner.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeckLock {
    key: DeckKey,
    cards: Vec<ResolvedCard>,
    digest: String,
}

impl ResolvedCard {
    pub fn key(&self) -> &CardKey {
        &self.key
    }

    pub fn definition(&self) -> &CardDefinitionRef {
        &self.definition
    }
}

impl DeckLock {
    pub fn key(&self) -> &DeckKey {
        &self.key
    }

    pub fn cards(&self) -> &[ResolvedCard] {
        &self.cards
    }

    pub fn digest(&self) -> &str {
        &self.digest
    }
}

/// A validation or exact-resolution failure while compiling a Deck.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeckCompileError {
    EmptyDeck,
    TooManyCards {
        actual: usize,
        maximum: usize,
    },
    InvalidField {
        field: String,
        reason: &'static str,
    },
    DuplicateCardKey {
        key: CardKey,
    },
    UnknownDefinition {
        card: CardKey,
        definition: CardDefinitionRef,
    },
}

impl fmt::Display for DeckCompileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyDeck => formatter.write_str("a Deck must contain at least one Card"),
            Self::TooManyCards { actual, maximum } => {
                write!(formatter, "Deck has {actual} Cards; maximum is {maximum}")
            }
            Self::InvalidField { field, reason } => {
                write!(formatter, "invalid {field}: {reason}")
            }
            Self::DuplicateCardKey { key } => {
                write!(formatter, "duplicate Card key `{key}`")
            }
            Self::UnknownDefinition { card, definition } => write!(
                formatter,
                "Card `{card}` references unknown exact definition `{definition}`"
            ),
        }
    }
}

impl Error for DeckCompileError {}

/// Compiles Decks against only the exact definitions supplied by composition.
#[derive(Clone, Debug)]
pub struct DeckCompiler {
    definitions: BTreeSet<CardDefinitionRef>,
}

impl DeckCompiler {
    pub fn new(
        definitions: impl IntoIterator<Item = CardDefinitionRef>,
    ) -> Result<Self, DeckCompileError> {
        let mut accepted = BTreeSet::new();

        for (index, definition) in definitions.into_iter().enumerate() {
            validate_field(
                &format!("definitions[{index}]"),
                definition.as_str(),
                MAX_DEFINITION_REF_BYTES,
            )?;
            accepted.insert(definition);
        }

        Ok(Self {
            definitions: accepted,
        })
    }

    pub fn compile(&self, spec: &DeckSpec) -> Result<DeckLock, DeckCompileError> {
        validate_field("DeckSpec.key", spec.key.as_str(), MAX_DECK_KEY_BYTES)?;

        if spec.cards.is_empty() {
            return Err(DeckCompileError::EmptyDeck);
        }
        if spec.cards.len() > MAX_CARDS_PER_DECK {
            return Err(DeckCompileError::TooManyCards {
                actual: spec.cards.len(),
                maximum: MAX_CARDS_PER_DECK,
            });
        }

        let mut card_keys = BTreeSet::new();
        let mut cards = Vec::with_capacity(spec.cards.len());

        for (index, card) in spec.cards.iter().enumerate() {
            validate_field(
                &format!("DeckSpec.cards[{index}].key"),
                card.key.as_str(),
                MAX_CARD_KEY_BYTES,
            )?;
            validate_field(
                &format!("DeckSpec.cards[{index}].definition"),
                card.definition.as_str(),
                MAX_DEFINITION_REF_BYTES,
            )?;

            if !card_keys.insert(card.key.clone()) {
                return Err(DeckCompileError::DuplicateCardKey {
                    key: card.key.clone(),
                });
            }
            if !self.definitions.contains(&card.definition) {
                return Err(DeckCompileError::UnknownDefinition {
                    card: card.key.clone(),
                    definition: card.definition.clone(),
                });
            }

            cards.push(ResolvedCard {
                key: card.key.clone(),
                definition: card.definition.clone(),
            });
        }

        cards.sort_unstable_by(|left, right| left.key.cmp(&right.key));
        let digest = deck_digest(&spec.key, &cards);

        Ok(DeckLock {
            key: spec.key.clone(),
            cards,
            digest,
        })
    }
}

fn validate_field(field: &str, value: &str, maximum_bytes: usize) -> Result<(), DeckCompileError> {
    if value.is_empty() {
        return Err(DeckCompileError::InvalidField {
            field: field.to_owned(),
            reason: "must not be empty",
        });
    }
    if value.trim() != value {
        return Err(DeckCompileError::InvalidField {
            field: field.to_owned(),
            reason: "must not have leading or trailing whitespace",
        });
    }
    if value.chars().any(char::is_control) {
        return Err(DeckCompileError::InvalidField {
            field: field.to_owned(),
            reason: "must not contain control characters",
        });
    }
    if value.len() > maximum_bytes {
        return Err(DeckCompileError::InvalidField {
            field: field.to_owned(),
            reason: "is too long",
        });
    }

    Ok(())
}

fn deck_digest(key: &DeckKey, cards: &[ResolvedCard]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    update_digest_field(&mut hasher, key.as_str());
    hasher.update((cards.len() as u64).to_be_bytes());

    for card in cards {
        update_digest_field(&mut hasher, card.key.as_str());
        update_digest_field(&mut hasher, card.definition.as_str());
    }

    let bytes = hasher.finalize();
    let mut digest = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut digest, "{byte:02x}").expect("writing to a String cannot fail");
    }
    digest
}

fn update_digest_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compilation_is_deterministic_and_rejects_invalid_compositions() {
        let agent = CardDefinitionRef::new("builtin.agent.echo@1");
        let sensor = CardDefinitionRef::new("builtin.sensor.fixed@1");
        let compiler = DeckCompiler::new([agent.clone(), sensor.clone()]).unwrap();

        let first = DeckSpec {
            key: DeckKey::new("assistant"),
            cards: vec![
                Card {
                    key: CardKey::new("sensor"),
                    definition: sensor.clone(),
                },
                Card {
                    key: CardKey::new("agent"),
                    definition: agent.clone(),
                },
            ],
        };
        let reordered = DeckSpec {
            key: first.key.clone(),
            cards: first.cards.iter().cloned().rev().collect(),
        };

        let first_lock = compiler.compile(&first).unwrap();
        let reordered_lock = compiler.compile(&reordered).unwrap();
        assert_eq!(first_lock, reordered_lock);
        assert_eq!(first_lock.cards()[0].key(), &CardKey::new("agent"));
        assert_eq!(
            first_lock.digest(),
            "d727c44fc39bd2a0d4c5b7546d32a6b3661343e3be44c71f3fda1b1c3cc77702"
        );

        let empty = DeckSpec {
            key: DeckKey::new("empty"),
            cards: Vec::new(),
        };
        assert_eq!(compiler.compile(&empty), Err(DeckCompileError::EmptyDeck));

        let too_many = DeckSpec {
            key: DeckKey::new("too-many"),
            cards: (0..=MAX_CARDS_PER_DECK)
                .map(|index| Card {
                    key: CardKey::new(format!("card-{index}")),
                    definition: agent.clone(),
                })
                .collect(),
        };
        assert_eq!(
            compiler.compile(&too_many),
            Err(DeckCompileError::TooManyCards {
                actual: MAX_CARDS_PER_DECK + 1,
                maximum: MAX_CARDS_PER_DECK,
            })
        );

        let duplicate = DeckSpec {
            key: DeckKey::new("duplicate"),
            cards: vec![
                Card {
                    key: CardKey::new("agent"),
                    definition: agent.clone(),
                },
                Card {
                    key: CardKey::new("agent"),
                    definition: sensor,
                },
            ],
        };
        assert_eq!(
            compiler.compile(&duplicate),
            Err(DeckCompileError::DuplicateCardKey {
                key: CardKey::new("agent")
            })
        );

        let unknown = DeckSpec {
            key: DeckKey::new("unknown"),
            cards: vec![Card {
                key: CardKey::new("agent"),
                definition: CardDefinitionRef::new("builtin.agent.missing@1"),
            }],
        };
        assert!(matches!(
            compiler.compile(&unknown),
            Err(DeckCompileError::UnknownDefinition { .. })
        ));

        let invalid = DeckSpec {
            key: DeckKey::new("invalid"),
            cards: vec![Card {
                key: CardKey::new(" agent"),
                definition: agent,
            }],
        };
        assert!(matches!(
            compiler.compile(&invalid),
            Err(DeckCompileError::InvalidField { .. })
        ));
    }
}
