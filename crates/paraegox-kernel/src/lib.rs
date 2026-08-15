//! Small, I/O-free identity primitives shared by the first Paraegox runtime path.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

const MAX_NODE_ID_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct NodeId(String);

impl NodeId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for NodeId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for NodeId {
    type Err = InvalidNodeId;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let valid_length = !value.is_empty() && value.len() <= MAX_NODE_ID_BYTES;
        let valid_start = value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric);
        let valid_characters = value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));

        if valid_length && valid_start && valid_characters {
            Ok(Self(value.to_owned()))
        } else {
            Err(InvalidNodeId)
        }
    }
}

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidNodeId;

impl Display for InvalidNodeId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "node id must be 1-{MAX_NODE_ID_BYTES} ASCII bytes, start with a letter or digit, and contain only letters, digits, '-' or '_'"
        )
    }
}

impl Error for InvalidNodeId {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RuntimeHostId(String);

impl RuntimeHostId {
    pub fn new(value: impl Into<String>) -> Result<Self, InvalidRuntimeHostId> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value.chars().all(|character| !character.is_control());
        if valid {
            Ok(Self(value))
        } else {
            Err(InvalidRuntimeHostId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for RuntimeHostId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RuntimeHostId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidRuntimeHostId;

impl Display for InvalidRuntimeHostId {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("runtime host id must be 1-128 bytes without control characters")
    }
}

impl Error for InvalidRuntimeHostId {}

macro_rules! runtime_uuid {
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

runtime_uuid!(NodeIncarnation);
runtime_uuid!(RuntimeHostEpoch);

#[cfg(test)]
mod tests {
    use super::NodeId;

    #[test]
    fn node_ids_are_safe_fabric_key_segments() {
        for valid in ["node-a", "edge_01", "A9"] {
            assert!(valid.parse::<NodeId>().is_ok(), "{valid} should be valid");
        }

        let too_long = "a".repeat(65);
        for invalid in ["", "-node", "node/a", "node a", "节点", &too_long] {
            assert!(
                invalid.parse::<NodeId>().is_err(),
                "{invalid} should be rejected"
            );
        }
    }
}
