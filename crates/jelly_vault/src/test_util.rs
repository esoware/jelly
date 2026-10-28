//! Vault fakes and fixtures, behind the `test-util` feature, which only
//! `[dev-dependencies]` enable.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use secrecy::{ExposeSecret as _, SecretString};
use serde::Deserialize as _;
use tempfile::TempDir;

use crate::disk;
use crate::entry::EntryId;
use crate::error::VaultError;
use crate::kdf::{Argon2Params, VaultPassword};
use crate::keychain::{MASTER_KEY_LEN, MasterKey, MasterKeyStore};
use crate::keys::header_mac_key;
use crate::vault::{UnlockedVault, Vault};
use crate::vault_file::VaultFile;

/// The password [`TempVault::create`] and [`TempVault::unlock`] use.
pub const TEST_PASSWORD: &str = "correct horse battery staple";

pub fn password(value: &str) -> VaultPassword {
    VaultPassword::new(SecretString::from(value))
}

/// Behaves like the real store as far as `jelly_vault` can tell, but forgets everything
/// on drop.
#[derive(Default)]
pub struct MemoryKeyStore {
    keys: Mutex<HashMap<String, [u8; MASTER_KEY_LEN]>>,
}

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Preloaded with a known master key.
    pub fn with_key(wallet_id: &str, key: [u8; MASTER_KEY_LEN]) -> Self {
        let store = Self::new();
        store.keys.lock().insert(wallet_id.to_owned(), key);
        store
    }

    pub fn is_empty(&self) -> bool {
        self.keys.lock().is_empty()
    }

    pub fn contains(&self, wallet_id: &str) -> bool {
        self.keys.lock().contains_key(wallet_id)
    }

    /// The stored key bytes, for copying a master key or recording it beside a fixture.
    pub fn key_bytes(&self, wallet_id: &str) -> Option<[u8; MASTER_KEY_LEN]> {
        self.keys.lock().get(wallet_id).copied()
    }
}

impl fmt::Debug for MemoryKeyStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemoryKeyStore({} keys, <redacted>)", self.keys.lock().len())
    }
}

impl MasterKeyStore for MemoryKeyStore {
    fn get(&self, wallet_id: &str) -> Result<Option<MasterKey>, VaultError> {
        Ok(self.keys.lock().get(wallet_id).copied().map(MasterKey::from_bytes))
    }

    fn put(&self, wallet_id: &str, key: &MasterKey) -> Result<(), VaultError> {
        self.keys.lock().insert(wallet_id.to_owned(), key.with_bytes(|bytes| *bytes));
        Ok(())
    }

    fn delete(&self, wallet_id: &str) -> Result<(), VaultError> {
        self.keys.lock().remove(wallet_id);
        Ok(())
    }
}

/// Fails every operation with the errors [`crate::KeyringStore`] reports for a locked
/// platform store, for paths that must not treat a broken keychain as a missing entry.
#[derive(Debug, Clone, Copy)]
pub struct FailingKeyStore;

impl FailingKeyStore {
    fn refusal() -> VaultError {
        VaultError::Keychain(keyring::Error::NoStorageAccess("the store is locked".into()))
    }
}

impl MasterKeyStore for FailingKeyStore {
    fn check_available(&self) -> Result<(), VaultError> {
        Err(VaultError::KeychainUnavailable)
    }

    fn get(&self, _wallet_id: &str) -> Result<Option<MasterKey>, VaultError> {
        Err(Self::refusal())
    }

    fn put(&self, _wallet_id: &str, _key: &MasterKey) -> Result<(), VaultError> {
        Err(Self::refusal())
    }

    fn delete(&self, _wallet_id: &str) -> Result<(), VaultError> {
        Err(Self::refusal())
    }
}

/// A vault handle over a temporary directory and an in-memory credential store.
///
/// Writes at [`Argon2Params::MINIMUM`], the real floor and not a weakened test profile,
/// so every fixture file opens in a shipping build.
#[derive(Debug)]
pub struct TempVault {
    /// Removes the directory on drop.
    _directory: TempDir,
    path: PathBuf,
    wallet_id: String,
    store: Arc<MemoryKeyStore>,
    vault: Vault,
}

impl TempVault {
    /// A handle over an empty store.
    pub fn new(wallet_id: &str) -> io::Result<Self> {
        Self::build(wallet_id, Arc::new(MemoryKeyStore::new()))
    }

    /// A handle whose store already holds `key`, so a committed fixture opens.
    pub fn with_master_key(wallet_id: &str, key: [u8; MASTER_KEY_LEN]) -> io::Result<Self> {
        Self::build(wallet_id, Arc::new(MemoryKeyStore::with_key(wallet_id, key)))
    }

    fn build(wallet_id: &str, store: Arc<MemoryKeyStore>) -> io::Result<Self> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("wallet.vault");
        let vault = Vault::new(
            path.clone(),
            wallet_id,
            Arc::<MemoryKeyStore>::clone(&store),
            Argon2Params::MINIMUM,
        );
        Ok(Self { _directory: directory, path, wallet_id: wallet_id.to_owned(), store, vault })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub const fn store(&self) -> &Arc<MemoryKeyStore> {
        &self.store
    }

    pub const fn vault(&self) -> &Vault {
        &self.vault
    }

    /// The same file under a second handle, for a different wallet id or KDF
    /// parameters.
    pub fn handle(&self, wallet_id: &str, kdf_params: Argon2Params) -> Vault {
        Vault::new(
            self.path.clone(),
            wallet_id,
            Arc::<MemoryKeyStore>::clone(&self.store),
            kdf_params,
        )
    }

    /// Create the vault under [`TEST_PASSWORD`].
    pub fn create(&self) -> Result<UnlockedVault, VaultError> {
        self.vault.create(&password(TEST_PASSWORD))
    }

    /// Unlock under [`TEST_PASSWORD`].
    pub fn unlock(&self) -> Result<UnlockedVault, VaultError> {
        self.vault.unlock(&password(TEST_PASSWORD))
    }

    /// Drop a prepared file into place.
    pub fn install(&self, bytes: &[u8]) -> Result<(), VaultError> {
        disk::write(&self.path, bytes)
    }

    pub fn read_file(&self) -> Result<Vec<u8>, VaultError> {
        fs::read(&self.path).map_err(|error| VaultError::io(&self.path, error))
    }

    /// The file as a mutable JSON document.
    pub fn forge(&self) -> Result<Forge, VaultError> {
        let json = serde_json::from_slice(&self.read_file()?).map_err(VaultError::Malformed)?;
        Ok(Forge { path: self.path.clone(), json })
    }

    pub fn master_key(&self) -> Option<MasterKey> {
        self.store.key_bytes(&self.wallet_id).map(MasterKey::from_bytes)
    }
}

/// The vault file as JSON, so a test can tamper with the bytes an attacker has and
/// re-stamp the tag as an attacker holding the master key could.
#[derive(Debug)]
pub struct Forge {
    path: PathBuf,
    json: serde_json::Value,
}

impl Forge {
    /// A field by JSON pointer, such as `/kdf/salt`.
    pub fn get(&self, pointer: &str) -> Option<&serde_json::Value> {
        self.json.pointer(pointer)
    }

    pub fn string(&self, pointer: &str) -> Option<&str> {
        self.get(pointer).and_then(serde_json::Value::as_str)
    }

    /// Replaces an existing field by JSON pointer. It never adds one, since the point is to
    /// mutate what the writer wrote.
    pub fn set(&mut self, pointer: &str, value: serde_json::Value) -> bool {
        match self.json.pointer_mut(pointer) {
            Some(slot) => {
                *slot = value;
                true
            }
            None => false,
        }
    }

    /// Flip the low bit of one byte of a hex-encoded field.
    pub fn flip_hex_byte(&mut self, pointer: &str, index: usize) -> bool {
        let Some(text) = self.string(pointer) else {
            return false;
        };
        let Ok(mut bytes) = hex::decode(text) else {
            return false;
        };
        let Some(byte) = bytes.get_mut(index) else {
            return false;
        };
        *byte ^= 0x01;
        self.set(pointer, serde_json::Value::String(hex::encode(bytes)))
    }

    /// Recompute the header tag, as an attacker holding the master key could.
    /// Fails if the mutation left the header unparsable.
    pub fn restamp(&mut self, master: &MasterKey) -> Result<(), VaultError> {
        let mut file = VaultFile::deserialize(&self.json).map_err(VaultError::Malformed)?;
        let key = header_mac_key(master, &file.hkdf.salt, &file.wallet_id)?;
        file.stamp(key.expose_secret())?;
        self.json =
            serde_json::to_value(&file).map_err(|_| VaultError::internal("forge encoding"))?;
        Ok(())
    }

    pub fn write(&self) -> Result<(), VaultError> {
        let bytes = serde_json::to_vec_pretty(&self.json)
            .map_err(|_| VaultError::internal("forge encoding"))?;
        disk::write(&self.path, &bytes)
    }
}

/// An id that is well formed but was never issued.
pub const fn missing_entry_id() -> EntryId {
    EntryId::from_bytes([0xee; EntryId::LEN])
}
