//! Encrypted per-wallet secret storage. Opening a vault requires both the OS keychain
//! master key and the password. Argon2id stretches the password, then HKDF combines both
//! factors.
//!
//! KDF work and file writes block, so async callers must use a blocking pool.
//!
//! A password-independent header MAC detects tampering before password checks. A commitment
//! distinguishes wrong passwords from AEAD corruption. There is no rollback protection or
//! unlock counter. With both the file and master key, an attacker can guess passwords
//! offline at Argon2id cost. This does not protect against malware running as the user
//! while unlocked.

#![deny(clippy::float_arithmetic)]
mod disk;
mod entry;
mod error;
mod kdf;
mod keychain;
mod keys;
mod payload;
#[cfg(feature = "test-util")]
pub mod test_util;
mod vault;
mod vault_file;

pub use crate::entry::{BlobContent, BlobKind, EntryId, SecretBlob};
pub use crate::error::VaultError;
pub use crate::kdf::{Argon2Params, VaultPassword};
pub use crate::keychain::{KeyringStore, MasterKey, MasterKeyStore};
pub use crate::vault::{UnlockedVault, Vault};
