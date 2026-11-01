use std::fmt;

use serde::{Deserialize, Serialize};

use crate::chain::ChainId;
use crate::script::ScriptKind;

/// A canonical address tagged with its chain. Use `AddressCodec::validate` for user input.
///
/// Not serializable. IPC and storage carry the address string alongside an existing chain
/// field.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Address {
    chain: ChainId,
    value: String,
}

impl Address {
    /// Trusts that `value` is canonical for `chain`. Use only for codec output, trusted
    /// database rows, or fixtures.
    pub fn assume_canonical(chain: ChainId, value: impl Into<String>) -> Self {
        Self { chain, value: value.into() }
    }

    pub const fn chain(&self) -> ChainId {
        self.chain
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn into_value(self) -> String {
        self.value
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.value)
    }
}

/// Each chain crate implements its codec, and its `ChainEntry` hands it out.
pub trait AddressCodec: Send + Sync + fmt::Debug {
    fn chain(&self) -> ChainId;

    /// Canonicalizes and validates `value`.
    fn validate(&self, value: &str) -> Result<Address, AddressError>;

    /// The chain's output script for `value`, for chains that have one.
    fn script_pubkey(&self, _value: &str) -> Result<Vec<u8>, AddressError> {
        Err(AddressError::NoScript { chain: self.chain() })
    }

    /// The script flavor of an already-valid address, for chains that have flavors.
    fn script_kind(&self, _value: &str) -> Option<ScriptKind> {
        None
    }

    /// The canonical address paying `script_pubkey`, the inverse of
    /// [`AddressCodec::script_pubkey`]. A script with no address form has none.
    fn address_of(&self, _script_pubkey: &[u8]) -> Option<String> {
        None
    }
}

/// Address failures carry positions and kinds, never a fragment of the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum AddressError {
    #[error("Enter an address for {}.", chain.display_name())]
    Empty { chain: ChainId },
    #[error(
        "The {} address has an invalid character at position {}.",
        chain.display_name(),
        position.saturating_add(1)
    )]
    Malformed { chain: ChainId, position: usize },
    /// The address decodes, but what it carries is unusable, so no character is at fault.
    #[error("The {} address is not valid. Check the address.", chain.display_name())]
    Invalid { chain: ChainId },
    #[error("The {} address checksum is incorrect. Check the address.", chain.display_name())]
    Checksum { chain: ChainId },
    #[error("The {} address has an invalid length. Check the address.", chain.display_name())]
    WrongLength { chain: ChainId },
    #[error("This address is not on the {} network.", chain.display_name())]
    WrongNetwork { chain: ChainId },
    #[error("The {} network does not use output scripts.", chain.display_name())]
    NoScript { chain: ChainId },
    /// A payment URI asks for something the payment depends on and Jelly can't send, such
    /// as a Solana Pay reference or a Monero payment ID.
    #[error(
        "This {} payment request needs something Jelly cannot send. Ask the recipient for a plain address.",
        chain.display_name()
    )]
    UnsupportedRequest { chain: ChainId },
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

/// Public address data the engine hands an adapter before each sync pass. It's never
/// persisted or sent over IPC. TON needs `pubkey` to deploy the wallet contract.
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
