use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

use crate::chain::ChainId;

/// A chain-tagged transaction id. An adapter renders it canonically, but a value from IPC
/// is unvalidated until the adapter parses it.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TxId {
    chain: ChainId,
    value: String,
}

impl TxId {
    pub fn new(chain: ChainId, value: impl Into<String>) -> Self {
        Self { chain, value: value.into() }
    }

    pub const fn chain(&self) -> ChainId {
        self.chain
    }

    pub fn value(&self) -> &str {
        &self.value
    }
}

impl fmt::Display for TxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

/// Single-use transaction authorization. Uses 128 random bits rendered as lowercase hex.
/// Parsing accepts either case.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingTxId(#[cfg_attr(feature = "ipc", specta(type = String))] [u8; Self::LEN]);

impl PendingTxId {
    pub const LEN: usize = 16;

    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for PendingTxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl Serialize for PendingTxId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for PendingTxId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let mut bytes = [0u8; Self::LEN];
        hex::decode_to_slice(&raw, &mut bytes)
            .map_err(|_| D::Error::custom("expected 32 hex digits"))?;
        Ok(Self(bytes))
    }
}
