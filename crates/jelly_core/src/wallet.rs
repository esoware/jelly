use std::fmt;

use serde::{Deserialize, Serialize};

use crate::address::Address;
use crate::chain::ChainId;
use crate::wire_enum;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Whether a wallet uses a BIP-39 passphrase and, if so, where it lives.
    pub enum PassphrasePolicy {
        None => "none",
        /// In use and kept in the vault, so unlock never asks for it.
        Stored => "stored",
        /// In use and never persisted, so every unlock asks for it.
        Prompt => "prompt",
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// How many words a generated BIP-39 phrase has.
    pub enum PhraseWords {
        Twelve => "twelve",
        TwentyFour => "twenty_four",
    }
}

/// Unvalidated derivation-path text stored and displayed as `m/84'/0'/0'/0/0`. Parse with
/// `jelly_keypair::parse_path` before use.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DerivationPath(String);

impl DerivationPath {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for DerivationPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where one address's key comes from.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeySource {
    #[serde(rename_all = "camelCase")]
    Derived { path: DerivationPath },
    /// A standalone key held in the vault under `vault_entry`.
    #[serde(rename_all = "camelCase")]
    Imported { vault_entry: String },
    /// A subaddress of the wallet's account on a chain whose addresses are subaddresses.
    /// One spend key covers them all.
    #[serde(rename_all = "camelCase")]
    Subaddress { major: u32, minor: u32 },
}

/// Public address data passed to adapters at the start of sync. It is neither persisted nor
/// sent over IPC. TON needs the public key to deploy an unfunded wallet contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedAddress {
    pub id: i64,
    pub address: Address,
    pub source: KeySource,
    pub pubkey: Option<[u8; 32]>,
}

impl WatchedAddress {
    pub const fn chain(&self) -> ChainId {
        self.address.chain()
    }
}
