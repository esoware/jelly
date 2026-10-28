use std::fmt;

use crate::chain::{ChainId, ScriptKind};

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
