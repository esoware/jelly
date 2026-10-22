use std::fmt;

use jelly_core::random_bytes;
use zeroize::Zeroizing;

use crate::error::VaultError;

/// Every Jelly credential lives under this service, with the wallet id as the account.
/// Changing it orphans every existing credential.
pub(crate) const KEYCHAIN_SERVICE: &str = "Jelly";

/// Nothing but this key goes in the OS store. The Windows Credential Manager caps a blob
/// at 2560 bytes, and a vault payload does not belong there anyway.
pub(crate) const MASTER_KEY_LEN: usize = 32;

/// The keychain-resident factor that a stolen vault file does not come with.
///
/// Wiped on drop and deliberately not `Clone`, `Display`, or serializable.
pub struct MasterKey(Zeroizing<[u8; MASTER_KEY_LEN]>);

impl MasterKey {
    pub(crate) fn generate() -> Result<Self, VaultError> {
        Ok(Self(random_bytes()?))
    }

    pub fn from_bytes(bytes: [u8; MASTER_KEY_LEN]) -> Self {
        Self(Zeroizing::new(bytes))
    }

    /// The width check stops a truncated or foreign credential from posing as a key
    /// that decrypts nothing.
    fn from_slice(bytes: &[u8]) -> Result<Self, VaultError> {
        <[u8; MASTER_KEY_LEN]>::try_from(bytes)
            .map(Self::from_bytes)
            .map_err(|_| VaultError::KeychainMalformed)
    }

    /// The closure keeps every read a visible call site. The bytes must not leave it in
    /// anything that does not zeroize.
    pub fn with_bytes<R>(&self, read: impl FnOnce(&[u8; MASTER_KEY_LEN]) -> R) -> R {
        read(&self.0)
    }
}

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

/// One credential per wallet. Every implementor upholds these invariants.
///
/// - `get` answers `Ok(None)` for a missing credential and an error for a
///   backend failure. Collapsing the two turns a broken keychain into an
///   apparently fresh install.
/// - `delete` of a missing credential is `Ok(())`, because the postcondition
///   already holds.
/// - `put` replaces whatever was there.
pub trait MasterKeyStore: Send + Sync + fmt::Debug {
    /// Probed once at startup, so a machine with no credential store fails there rather
    /// than at the first unlock.
    fn check_available(&self) -> Result<(), VaultError> {
        Ok(())
    }

    fn get(&self, wallet_id: &str) -> Result<Option<MasterKey>, VaultError>;

    fn put(&self, wallet_id: &str, key: &MasterKey) -> Result<(), VaultError>;

    fn delete(&self, wallet_id: &str) -> Result<(), VaultError>;
}

/// The OS credential store. `keyring`'s `v1` feature selects Windows Credential Manager,
/// macOS Keychain Services, or the zbus Secret Service client.
#[derive(Debug, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    fn entry(wallet_id: &str) -> Result<keyring::Entry, VaultError> {
        keyring::Entry::new(KEYCHAIN_SERVICE, wallet_id).map_err(VaultError::Keychain)
    }
}

impl MasterKeyStore for KeyringStore {
    fn check_available(&self) -> Result<(), VaultError> {
        match keyring::Entry::store_status() {
            Ok(()) => Ok(()),
            Err(_) => Err(VaultError::KeychainUnavailable),
        }
    }

    fn get(&self, wallet_id: &str) -> Result<Option<MasterKey>, VaultError> {
        match Self::entry(wallet_id)?.get_secret() {
            Ok(secret) => MasterKey::from_slice(&Zeroizing::new(secret)).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(VaultError::Keychain(error)),
        }
    }

    fn put(&self, wallet_id: &str, key: &MasterKey) -> Result<(), VaultError> {
        let entry = Self::entry(wallet_id)?;
        key.with_bytes(|bytes| entry.set_secret(bytes)).map_err(VaultError::Keychain)
    }

    fn delete(&self, wallet_id: &str) -> Result<(), VaultError> {
        match Self::entry(wallet_id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(VaultError::Keychain(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_master_keys_differ_and_redact() {
        let first = MasterKey::generate().unwrap();
        let second = MasterKey::generate().unwrap();
        assert_ne!(first.with_bytes(|key| *key), second.with_bytes(|key| *key));
        assert_ne!(first.with_bytes(|key| *key), [0u8; MASTER_KEY_LEN]);
        assert_eq!(format!("{first:?}"), "MasterKey(<redacted>)");
    }

    #[test]
    fn only_a_full_width_credential_is_a_master_key() {
        let full = vec![7u8; MASTER_KEY_LEN];
        assert_eq!(
            MasterKey::from_slice(&full).unwrap().with_bytes(|key| *key),
            [7u8; MASTER_KEY_LEN]
        );
        for length in [0, 1, MASTER_KEY_LEN - 1, MASTER_KEY_LEN + 1, 64] {
            assert!(
                matches!(
                    MasterKey::from_slice(&vec![7u8; length]),
                    Err(VaultError::KeychainMalformed)
                ),
                "a {length}-byte credential must be refused"
            );
        }
    }

    /// Editing either orphans every credential already in the platform store.
    #[test]
    fn the_keychain_constants_are_pinned() {
        assert_eq!(KEYCHAIN_SERVICE, "Jelly");
        assert_eq!(MASTER_KEY_LEN, 32);
    }
}
