use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::mem;
use std::path::PathBuf;
use std::sync::Arc;

use jelly_core::KeyMaterial;
use parking_lot::Mutex;
use secrecy::ExposeSecret as _;

use crate::disk;
use crate::entry::{BlobKind, EntryId, SecretBlob};
use crate::error::VaultError;
use crate::kdf::{Argon2Params, VaultPassword};
use crate::keychain::{MasterKey, MasterKeyStore};
use crate::keys::{SessionKeys, header_mac_key};
use crate::payload;
use crate::vault_file::VaultFile;

/// A locked vault. Holds no key material and does no I/O until a method is called.
///
/// Methods that touch the file block, because the KDF takes hundreds of milliseconds by
/// design and each write seals and renames the whole file. Async callers run them in
/// `spawn_blocking`.
#[derive(Debug, Clone)]
pub struct Vault {
    path: PathBuf,
    wallet_id: String,
    store: Arc<dyn MasterKeyStore>,
    kdf_params: Argon2Params,
}

impl Vault {
    /// `kdf_params` are what this handle writes and upgrades to. The
    /// [`Argon2Params::MINIMUM`] floor still applies.
    pub fn new(
        path: impl Into<PathBuf>,
        wallet_id: impl Into<String>,
        store: Arc<dyn MasterKeyStore>,
        kdf_params: Argon2Params,
    ) -> Self {
        Self { path: path.into(), wallet_id: wallet_id.into(), store, kdf_params }
    }

    /// Whether the file or its staged copy exists, not whether either opens.
    fn has_file(&self) -> bool {
        self.path.is_file() || self.staged_path().is_file()
    }

    /// Whether [`Vault::create`] would refuse, because the file, its staged copy, or the
    /// keychain master key is already there.
    pub fn is_claimed(&self) -> Result<bool, VaultError> {
        Ok(self.has_file() || self.store.get(&self.wallet_id)?.is_some())
    }

    /// Refuses to overwrite an existing file, its staged copy, or the keychain master
    /// key. A key with no file at this path may still be the only key for a copy that was
    /// moved or backed up elsewhere.
    ///
    /// The platform keychain has no create-if-absent, so provisioning is serialized within
    /// this process and the app runs as a single instance.
    pub fn create(&self, password: &VaultPassword) -> Result<UnlockedVault, VaultError> {
        static PROVISIONING: Mutex<()> = Mutex::new(());
        let _provisioning = PROVISIONING.lock();
        if self.has_file() {
            return Err(VaultError::AlreadyExists(self.path.clone()));
        }
        if self.store.get(&self.wallet_id)?.is_some() {
            return Err(VaultError::KeychainOccupied);
        }

        let master = MasterKey::generate()?;
        self.store.put(&self.wallet_id, &master)?;
        let unlocked = self.provision(&master, password).inspect_err(|_| {
            // No file was sealed under the key just put, so nothing needs it.
            if let Err(cleanup_error) = self.store.delete(&self.wallet_id) {
                tracing::warn!(error = %cleanup_error, "couldn't remove the orphaned master key");
            }
        })?;
        tracing::info!("vault created");
        Ok(unlocked)
    }

    /// Opens the vault with both factors. The checks run in this order.
    ///
    /// 1. Parse the file and verify its wallet id.
    /// 2. Load the keychain master key.
    /// 3. Verify the password-independent header MAC. Failure means corruption.
    /// 4. Compare the password commitment. Failure means a wrong password.
    /// 5. Decrypt with AEAD. Failure now means corruption.
    ///
    /// Any KDF parameter below the handle's is raised to it, and none is ever lowered. A
    /// failed upgrade doesn't fail an otherwise valid unlock.
    pub fn unlock(&self, password: &VaultPassword) -> Result<UnlockedVault, VaultError> {
        let bytes = fs::read(&self.path).map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                VaultError::FileNotFound(self.path.clone())
            } else {
                VaultError::io(&self.path, error)
            }
        })?;
        let file = VaultFile::parse(&bytes)?;
        if file.wallet_id != self.wallet_id {
            return Err(VaultError::corrupt("the file belongs to a different wallet"));
        }

        let master = self.master()?;
        let derivation = file.derivation();
        let mac_key = header_mac_key(&master, &derivation.hkdf_salt, &self.wallet_id)?;
        file.verify_integrity(mac_key.expose_secret())?;

        let password_keys = derivation.password_keys(&master, password, &self.wallet_id)?;
        drop(master);
        password_keys.check_commitment(&file.commitment)?;

        let keys = SessionKeys::new(&password_keys, mac_key, derivation);
        let plaintext = file.open(&keys.cipher)?;
        let entries = payload::decode(&plaintext)?;
        drop(plaintext);

        let mut session = UnlockedVault { handle: self.clone(), keys, entries };
        let raised = derivation.kdf_params.at_least(self.kdf_params);
        if raised != derivation.kdf_params {
            match session.rekey(password, raised) {
                Ok(()) => tracing::info!("vault kdf parameters upgraded"),
                Err(error) => tracing::warn!(%error, "couldn't upgrade the vault kdf parameters"),
            }
        }
        Ok(session)
    }

    /// Checks `password` before [`Vault::destroy`]. If an interrupted destroy already
    /// removed the file, the staged copy checks it instead, and once the master key is
    /// gone too, only cleanup remains. A missing file alone never authorizes deleting a key
    /// that could belong to a moved vault.
    pub fn authorize_destruction(&self, password: &VaultPassword) -> Result<(), VaultError> {
        let refusal = match self.unlock(password) {
            Ok(vault) => {
                vault.lock();
                return Ok(());
            }
            Err(error @ (VaultError::FileNotFound(_) | VaultError::KeychainMissing)) => error,
            Err(error) => return Err(error),
        };
        let staged = Self { path: self.staged_path(), ..self.clone() };
        match staged.unlock(password) {
            Ok(vault) => {
                vault.lock();
                Ok(())
            }
            Err(VaultError::KeychainMissing) => Ok(()),
            Err(VaultError::FileNotFound(_)) => {
                if self.is_claimed()? {
                    Err(refusal)
                } else {
                    Ok(())
                }
            }
            Err(error) => Err(error),
        }
    }

    /// Removes the vault file and its keychain master key. The caller must first pass
    /// [`Vault::authorize_destruction`] or [`UnlockedVault::verify_password`], unless it's
    /// rolling back an unfinished creation. A synced staged copy outlives both removals, so
    /// a failure partway through leaves something to authorize the retry.
    pub fn destroy(&self) -> Result<(), VaultError> {
        let staged = self.staged_path();
        match fs::read(&self.path) {
            Ok(bytes) => disk::write(&staged, &bytes)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(VaultError::io(&self.path, error)),
        }
        disk::remove_if_present(&self.path)?;
        self.store.delete(&self.wallet_id)?;
        disk::remove_if_present(&staged)?;
        tracing::warn!("vault destroyed");
        Ok(())
    }

    fn staged_path(&self) -> PathBuf {
        self.path.with_added_extension("deleting")
    }

    fn provision(
        &self,
        master: &MasterKey,
        password: &VaultPassword,
    ) -> Result<UnlockedVault, VaultError> {
        let keys = SessionKeys::derive(master, password, &self.wallet_id, self.kdf_params)?;
        let session = UnlockedVault { handle: self.clone(), keys, entries: BTreeMap::new() };
        disk::create(&self.path, &session.seal()?)?;
        Ok(session)
    }

    fn master(&self) -> Result<MasterKey, VaultError> {
        self.store.get(&self.wallet_id)?.ok_or(VaultError::KeychainMissing)
    }
}

/// An open session. The session AEAD and the decrypted entries are wiped when this
/// value drops.
///
/// Every mutation rewrites the whole file under a fresh nonce before it returns, and
/// rolls memory back if the write fails, so memory and disk never diverge.
pub struct UnlockedVault {
    handle: Vault,
    keys: SessionKeys,
    entries: BTreeMap<EntryId, SecretBlob>,
}

impl fmt::Debug for UnlockedVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnlockedVault")
            .field("wallet_id", &self.handle.wallet_id)
            .field("entries", &self.entries.len())
            .finish_non_exhaustive()
    }
}

impl UnlockedVault {
    /// The parameters the file on disk carries. After an upgrade they meet the handle's
    /// on every axis.
    #[cfg(feature = "test-util")]
    pub const fn kdf_params(&self) -> Argon2Params {
        self.keys.derivation.kdf_params
    }

    #[cfg(feature = "test-util")]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    #[cfg(feature = "test-util")]
    pub fn read(&self, id: EntryId) -> Result<SecretBlob, VaultError> {
        self.entries.get(&id).map(SecretBlob::duplicate).ok_or(VaultError::EntryNotFound(id))
    }

    /// Consuming `self` drops every key and decrypted secret, so no handle can read one
    /// afterward.
    pub fn lock(self) {
        drop(self);
    }

    /// Every entry of `kind`, in id order. A borrowed blob is readable only through
    /// [`SecretBlob::with_bytes`], so looking at one copies nothing.
    pub fn entries_of_kind(&self, kind: BlobKind) -> impl Iterator<Item = (EntryId, &SecretBlob)> {
        self.entries
            .iter()
            .filter(move |(_, blob)| blob.kind() == kind)
            .map(|(id, blob)| (*id, blob))
    }

    /// The lowest-id entry of `kind`. A wallet holds at most one seed, one
    /// stored passphrase and one verifier, so this is how they are found.
    fn find(&self, kind: BlobKind) -> Option<(EntryId, &SecretBlob)> {
        self.entries_of_kind(kind).next()
    }

    pub fn read_first(&self, kind: BlobKind) -> Option<SecretBlob> {
        self.find(kind).map(|(_, blob)| blob.duplicate())
    }

    /// For a secret that leaves the application. The password is re-derived and checked
    /// here, so no caller can skip re-authentication.
    ///
    /// Addressed by kind, so it serves the kinds a wallet holds at most one of, such as
    /// the seed entropy.
    pub fn export(
        &self,
        password: &VaultPassword,
        kind: BlobKind,
    ) -> Result<SecretBlob, VaultError> {
        self.verify_password(password)?;
        let (id, blob) = self.find(kind).ok_or(VaultError::NoEntryOfKind(kind))?;
        tracing::warn!(entry = %id, ?kind, "vault secret exported");
        Ok(blob.duplicate())
    }

    /// For keys the session is already authorized for. A caller that hands a secret out
    /// re-authenticates first, through [`UnlockedVault::export`] or
    /// [`UnlockedVault::verify_password`].
    pub fn key_material(&self, id: EntryId) -> Result<KeyMaterial, VaultError> {
        let blob = self.entries.get(&id).ok_or(VaultError::EntryNotFound(id))?;
        blob.duplicate().into_key_material().ok_or(VaultError::NotKeyMaterial(id))
    }

    /// Compares a re-derived commitment in constant time with the one this session
    /// opened under. Costs a full KDF pass, which is the point.
    pub fn verify_password(&self, password: &VaultPassword) -> Result<(), VaultError> {
        let master = self.handle.master()?;
        let password_keys =
            self.keys.derivation.password_keys(&master, password, &self.handle.wallet_id)?;
        drop(master);
        password_keys.check_commitment(&self.keys.commitment)
    }

    /// Takes ownership, so the secret isn't copied on its way in.
    pub fn insert(&mut self, blob: SecretBlob) -> Result<EntryId, VaultError> {
        let id = EntryId::generate()?;
        self.entries.insert(id, blob);
        self.persist().inspect_err(|_| {
            self.entries.remove(&id);
        })?;
        Ok(id)
    }

    /// Fails on a missing id instead of inserting.
    pub fn replace(&mut self, id: EntryId, blob: SecretBlob) -> Result<(), VaultError> {
        let slot = self.entries.get_mut(&id).ok_or(VaultError::EntryNotFound(id))?;
        let previous = mem::replace(slot, blob);
        self.persist().inspect_err(|_| {
            self.entries.insert(id, previous);
        })
    }

    /// Removed from the current file, which is re-encrypted under a fresh nonce. Earlier
    /// copies of the file still hold it.
    pub fn remove(&mut self, id: EntryId) -> Result<(), VaultError> {
        let removed = self.entries.remove(&id).ok_or(VaultError::EntryNotFound(id))?;
        self.persist().inspect_err(|_| {
            self.entries.insert(id, removed);
        })?;
        tracing::warn!(entry = %id, "vault entry destroyed");
        Ok(())
    }

    /// Rotates the password factor with fresh salts, commitment, and nonce, and rewrites
    /// the whole file. The keychain master key stays the same.
    ///
    /// Parameters are raised as in [`Vault::unlock`], so a rotation never weakens a
    /// vault.
    pub fn change_password(
        &mut self,
        current: &VaultPassword,
        new: &VaultPassword,
    ) -> Result<(), VaultError> {
        self.verify_password(current)?;
        self.rekey(new, self.keys.derivation.kdf_params.at_least(self.handle.kdf_params))?;
        tracing::info!("vault password changed");
        Ok(())
    }

    fn rekey(
        &mut self,
        password: &VaultPassword,
        kdf_params: Argon2Params,
    ) -> Result<(), VaultError> {
        let master = self.handle.master()?;
        let keys = SessionKeys::derive(&master, password, &self.handle.wallet_id, kdf_params)?;
        drop(master);
        let previous = mem::replace(&mut self.keys, keys);
        self.persist().inspect_err(|_| self.keys = previous)
    }

    fn persist(&self) -> Result<(), VaultError> {
        disk::write(&self.handle.path, &self.seal()?)
    }

    fn seal(&self) -> Result<Vec<u8>, VaultError> {
        let plaintext = payload::encode(&self.entries)?;
        VaultFile::seal(&self.handle.wallet_id, &self.keys, plaintext)?.to_bytes()
    }
}
