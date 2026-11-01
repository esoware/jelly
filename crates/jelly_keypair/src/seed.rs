use std::fmt;

use bitcoin::bip32;
use jelly_core::{KeyError, KeyKind, KeyMaterial};
use secrecy::{ExposeSecret as _, SecretBox};

use crate::bip32::Bip32Node;
use crate::ed25519::Ed25519Key;
use crate::monero_spend::monero_spend_material;
use crate::secp256k1::Secp256k1Key;
use crate::slip10::Slip10Node;

/// A 64-byte BIP-39 seed, wiped on drop and deliberately not `Clone`. Every derivation
/// from it is a pure function of the seed and its arguments.
///
/// Built in place on the heap so the finished seed is never copied through the stack.
///
/// The session holds the expanded seed, not the entropy and passphrase. Re-deriving per
/// call would keep both alive anyway and add a PBKDF2 pass to every address.
pub struct Seed(SecretBox<[u8; SEED_BYTES]>);

const SEED_BYTES: usize = 64;

impl Seed {
    pub const LEN: usize = SEED_BYTES;

    /// Takes a seed already on the heap, so the caller's buffer is the only copy.
    pub fn from_boxed(bytes: Box<[u8; Self::LEN]>) -> Self {
        Self(SecretBox::new(bytes))
    }

    /// Copies a seed a test vector names, to derive from it without a phrase.
    #[cfg(feature = "test-util")]
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyError> {
        if bytes.len() != Self::LEN {
            return Err(KeyError::InvalidSeedLength { got: bytes.len() });
        }
        let mut boxed: Box<[u8; Self::LEN]> = Box::new([0u8; Self::LEN]);
        boxed.copy_from_slice(bytes);
        Ok(Self::from_boxed(boxed))
    }

    /// The closure keeps every use a visible call site and hands out no copy a caller
    /// could leave unwiped.
    pub fn with_bytes<R>(&self, read: impl FnOnce(&[u8; Self::LEN]) -> R) -> R {
        read(self.0.expose_secret())
    }

    fn bip32_at(&self, path: &bip32::DerivationPath) -> Result<Bip32Node, KeyError> {
        self.with_bytes(|bytes| Bip32Node::from_seed_bytes(bytes))?.derive(path)
    }

    fn slip10_at(&self, path: &bip32::DerivationPath) -> Result<Slip10Node, KeyError> {
        self.with_bytes(|bytes| Slip10Node::from_seed_bytes(bytes))?.derive(path)
    }

    /// The `xpub` at `path`, for watch-only export.
    pub fn xpub_at(&self, path: &bip32::DerivationPath) -> Result<String, KeyError> {
        Ok(self.bip32_at(path)?.to_xpub_string())
    }

    pub fn secp256k1_at(&self, path: &bip32::DerivationPath) -> Result<Secp256k1Key, KeyError> {
        self.bip32_at(path)?.to_secp256k1_key()
    }

    pub fn ed25519_at(&self, path: &bip32::DerivationPath) -> Result<Ed25519Key, KeyError> {
        Ok(self.slip10_at(path)?.to_ed25519_key())
    }

    /// Derives at `path` with the scheme `kind` names.
    ///
    /// Any path the curve can walk is accepted, including a custom depth or index, so keys
    /// another wallet made on an unusual path can still be restored.
    pub fn key_material_at(
        &self,
        kind: KeyKind,
        path: &bip32::DerivationPath,
    ) -> Result<KeyMaterial, KeyError> {
        match kind {
            KeyKind::Secp256k1 => Ok(self.secp256k1_at(path)?.key_material()),
            KeyKind::Ed25519Seed => Ok(self.ed25519_at(path)?.key_material()),
            KeyKind::MoneroSpend => Ok(self.slip10_at(path)?.with_key_bytes(monero_spend_material)),
        }
    }
}

impl fmt::Debug for Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Seed(<redacted>)")
    }
}
