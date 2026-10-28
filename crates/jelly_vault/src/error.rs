use std::io;
use std::path::PathBuf;

use jelly_core::{CoreError, EntropyError};

use crate::entry::{BlobKind, EntryId};
use crate::keychain::{KEYCHAIN_SERVICE, MASTER_KEY_LEN};

/// Never exposes secret fragments through display or source errors. Paths, lengths,
/// counts, ids, and fixed reasons are allowed.
#[derive(Debug, thiserror::Error)]
pub enum VaultError {
    #[error("wrong password")]
    WrongPassword,

    #[error("no vault file at {0}")]
    FileNotFound(PathBuf),

    #[error("a vault already exists at {0}")]
    AlreadyExists(PathBuf),

    #[error("no keychain entry for this wallet in service '{KEYCHAIN_SERVICE}'")]
    KeychainMissing,

    #[error("the keychain already holds a master key for this wallet")]
    KeychainOccupied,

    #[error("the keychain entry for this wallet isn't a {MASTER_KEY_LEN}-byte master key")]
    KeychainMalformed,

    #[error("the platform credential store didn't initialize")]
    KeychainUnavailable,

    #[error("the platform credential store refused the operation")]
    Keychain(#[source] keyring::Error),

    #[error("the vault file isn't readable as a vault")]
    Malformed(#[source] serde_json::Error),

    #[error("the vault file is corrupt or has been tampered with: {reason}")]
    Corrupt { reason: &'static str },

    #[error("unsupported vault format version {found}; this build reads {supported}")]
    UnsupportedVersion { found: u32, supported: u32 },

    #[error("unsupported vault plaintext version {found}; this build reads {supported}")]
    UnsupportedPayloadVersion { found: u8, supported: u8 },

    #[error("argon2id parameters below the accepted floor: {field}")]
    WeakKdfParams { field: &'static str },

    #[error("invalid argon2id parameters")]
    KdfParams(#[from] argon2::Error),

    #[error("a vault entry id is {} hex characters", EntryId::LEN.saturating_mul(2))]
    InvalidEntryId,

    #[error("no vault entry with id {0}")]
    EntryNotFound(EntryId),

    #[error("this vault holds no {0} entry")]
    NoEntryOfKind(BlobKind),

    #[error("vault entry {0} doesn't hold signing key material")]
    NotKeyMaterial(EntryId),

    #[error("io error on {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("couldn't draw random bytes for the vault")]
    Entropy(#[from] EntropyError),

    #[error("internal: {context}")]
    Internal { context: &'static str },
}

impl VaultError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        Self::Io { path: path.into(), source }
    }

    /// A broken invariant inside this crate.
    pub(crate) const fn internal(context: &'static str) -> Self {
        Self::Internal { context }
    }

    pub(crate) const fn corrupt(reason: &'static str) -> Self {
        Self::Corrupt { reason }
    }
}

impl From<VaultError> for CoreError {
    fn from(error: VaultError) -> Self {
        match error {
            VaultError::WrongPassword => Self::WrongPassword,
            // Asking a non-key entry for key material reads as no key under that id.
            VaultError::EntryNotFound(id) | VaultError::NotKeyMaterial(id) => {
                Self::not_found("vault entry", id.to_string())
            }
            VaultError::NoEntryOfKind(kind) => Self::not_found("vault entry", kind.as_str()),
            VaultError::InvalidEntryId => {
                Self::invalid_input("vault entry", "Choose a valid saved key.")
            }
            VaultError::AlreadyExists(_) | VaultError::KeychainOccupied => {
                Self::AlreadyExists { entity: "vault" }
            }
            error @ (VaultError::Keychain(_)
            | VaultError::KeychainUnavailable
            | VaultError::KeychainMissing
            | VaultError::KeychainMalformed) => Self::keychain("the vault keychain entry", error),
            error @ (VaultError::FileNotFound(_)
            | VaultError::Malformed(_)
            | VaultError::Corrupt { .. }
            | VaultError::UnsupportedVersion { .. }
            | VaultError::UnsupportedPayloadVersion { .. }
            | VaultError::WeakKdfParams { .. }
            | VaultError::KdfParams(_)
            | VaultError::Io { .. }
            | VaultError::Entropy(_)
            | VaultError::Internal { .. }) => Self::internal("the vault file", error),
        }
    }
}
