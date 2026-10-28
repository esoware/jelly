use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io::ErrorKind;
use std::mem;
use std::path::PathBuf;
use std::sync::Arc;

use jelly_core::{KeyMaterial, random_bytes};
use parking_lot::Mutex;
use secrecy::{ExposeSecret as _, SecretBox};

use crate::disk;
use crate::entry::{BlobKind, EntryId, SecretBlob};
use crate::error::VaultError;
use crate::kdf::{Argon2Params, KDF_SALT_LEN, VaultPassword, hash_password};
use crate::keychain::{MasterKey, MasterKeyStore};
use crate::keys::{
    COMMITMENT_LEN, HKDF_SALT_LEN, KEY_LEN, NONCE_LEN, SessionCipher, commitment_matches,
    header_mac_key, password_keys,
};
use crate::payload;
use crate::vault_file::{AeadBlock, HkdfBlock, KdfBlock, VaultFile, associated_data};

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
        let mac_key = header_mac_key(&master, &file.hkdf.salt, &self.wallet_id)?;
        file.verify_integrity(mac_key.expose_secret())?;

        let kdf_params = file.kdf.params();
        let hash = hash_password(password, &file.kdf.salt, kdf_params)?;
        let derived = password_keys(&master, &hash, &file.hkdf.salt, &self.wallet_id)?;
        drop(hash);
        drop(master);
        if !commitment_matches(&derived.commitment, &file.commitment) {
            return Err(VaultError::WrongPassword);
        }

        let cipher = Box::new(SessionCipher::new(&derived.aead_key));
        let aad = associated_data(&self.wallet_id)?;
        let plaintext = cipher
            .open(&file.aead.nonce, &aad, &file.ciphertext)
            .ok_or_else(|| VaultError::corrupt("the ciphertext doesn't open under a proven key"))?;
        let entries = payload::decode(&plaintext)?;
        drop(plaintext);

        let mut session = UnlockedVault {
            handle: self.clone(),
            keys: SessionKeys {
                cipher,
                mac_key,
                commitment: derived.commitment,
                kdf_params,
                kdf_salt: file.kdf.salt,
                hkdf_salt: file.hkdf.salt,
            },
            entries,
        };
        let raised = kdf_params.at_least(self.kdf_params);
        if raised != kdf_params {
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

/// The keys a session seals under, and the header values that go with them.
///
/// The cipher and the header MAC key are boxed. An [`UnlockedVault`] is returned by
/// value, and a move is a memcpy, so an inline key would leave an unwiped copy at every
/// address the session has had.
struct SessionKeys {
    cipher: Box<SessionCipher>,
    mac_key: SecretBox<[u8; KEY_LEN]>,
    /// Not secret, since the header holds the same value.
    commitment: [u8; COMMITMENT_LEN],
    kdf_params: Argon2Params,
    kdf_salt: [u8; KDF_SALT_LEN],
    hkdf_salt: [u8; HKDF_SALT_LEN],
}

impl SessionKeys {
    /// Both factors under fresh salts, for a new file or a rekey.
    fn derive(
        master: &MasterKey,
        password: &VaultPassword,
        wallet_id: &str,
        kdf_params: Argon2Params,
    ) -> Result<Self, VaultError> {
        let kdf_salt = *random_bytes::<KDF_SALT_LEN>()?;
        let hkdf_salt = *random_bytes::<HKDF_SALT_LEN>()?;
        let hash = hash_password(password, &kdf_salt, kdf_params)?;
        let derived = password_keys(master, &hash, &hkdf_salt, wallet_id)?;
        drop(hash);
        let mac_key = header_mac_key(master, &hkdf_salt, wallet_id)?;
        Ok(Self {
            cipher: Box::new(SessionCipher::new(&derived.aead_key)),
            mac_key,
            commitment: derived.commitment,
            kdf_params,
            kdf_salt,
            hkdf_salt,
        })
    }
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
        self.keys.kdf_params
    }

    #[cfg(feature = "test-util")]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
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

    /// For secrets the session is already authorized for. A caller that hands a secret
    /// out re-authenticates first, through [`UnlockedVault::export`] or
    /// [`UnlockedVault::verify_password`].
    pub fn read(&self, id: EntryId) -> Result<SecretBlob, VaultError> {
        self.entries.get(&id).map(SecretBlob::duplicate).ok_or(VaultError::EntryNotFound(id))
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

    pub fn key_material(&self, id: EntryId) -> Result<KeyMaterial, VaultError> {
        self.read(id)?.into_key_material().ok_or(VaultError::NotKeyMaterial(id))
    }

    /// Compares a re-derived commitment in constant time with the one this session
    /// opened under. Costs a full KDF pass, which is the point.
    pub fn verify_password(&self, password: &VaultPassword) -> Result<(), VaultError> {
        let master = self.handle.master()?;
        let hash = hash_password(password, &self.keys.kdf_salt, self.keys.kdf_params)?;
        let derived = password_keys(&master, &hash, &self.keys.hkdf_salt, &self.handle.wallet_id)?;
        drop(hash);
        drop(master);
        if commitment_matches(&derived.commitment, &self.keys.commitment) {
            Ok(())
        } else {
            Err(VaultError::WrongPassword)
        }
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
        self.rekey(new, self.keys.kdf_params.at_least(self.handle.kdf_params))?;
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
        let keys = &self.keys;
        let aad = associated_data(&self.handle.wallet_id)?;
        let nonce = *random_bytes::<NONCE_LEN>()?;
        let ciphertext = keys.cipher.seal(&nonce, &aad, payload::encode(&self.entries)?)?;
        let file = VaultFile::build(
            &self.handle.wallet_id,
            KdfBlock::new(keys.kdf_params, keys.kdf_salt),
            HkdfBlock::new(keys.hkdf_salt),
            AeadBlock::new(nonce),
            keys.commitment,
            ciphertext,
            keys.mac_key.expose_secret(),
        )?;
        file.to_bytes()
    }
}
