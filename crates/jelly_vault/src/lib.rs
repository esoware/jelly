//! Encrypted per-wallet secret storage. Opening a vault requires both the OS keychain
//! master key and the password. Argon2id stretches the password, then HKDF combines both
//! factors.
//!
//! KDF work and file writes block, so async callers must use a blocking pool.
//!
//! A header MAC that doesn't depend on the password catches tampering before the password
//! is checked, and a key commitment tells a wrong password apart from a corrupt ciphertext.
//! There's no rollback protection or unlock counter, so anyone holding both the file and
//! the master key can guess passwords offline at Argon2id cost. Malware running as the
//! user while the vault is unlocked is out of scope.

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
