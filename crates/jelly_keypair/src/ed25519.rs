use std::fmt;

use ed25519_dalek::{Signer as _, SigningKey};
use jelly_core::{KeyError, KeyKind, KeyMaterial};
use zeroize::Zeroizing;

/// `seed` is the 32-byte private seed, the first half of a Solana keypair file.
///
/// The public key is always recomputed from it, never accepted from a caller, because
/// signing with a mismatched pair can leak the private scalar across two signatures.
pub struct Ed25519Key {
    seed: Zeroizing<[u8; 32]>,
    public: [u8; 32],
}

impl Ed25519Key {
    pub fn from_seed_bytes(seed: &[u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(seed);
        let public = signing.verifying_key().to_bytes();
        Self { seed: Zeroizing::new(*seed), public }
    }

    /// The key `material` holds, refusing material of another kind or length.
    pub fn from_key_material(material: &KeyMaterial) -> Result<Self, KeyError> {
        if material.kind() != KeyKind::Ed25519Seed {
            return Err(KeyError::InvalidSecretKey);
        }
        material.with_bytes(|bytes| {
            let seed = Zeroizing::new(
                <[u8; 32]>::try_from(bytes)
                    .map_err(|_| KeyError::InvalidKeyLength { got: bytes.len() })?,
            );
            Ok(Self::from_seed_bytes(&seed))
        })
    }

    pub fn with_secret_bytes<R>(&self, read: impl FnOnce(&[u8; 32]) -> R) -> R {
        read(&self.seed)
    }

    pub const fn public_bytes(&self) -> [u8; 32] {
        self.public
    }

    /// The 64-byte `secret || public` layout of a Solana keypair file.
    pub fn to_keypair_bytes(&self) -> Zeroizing<[u8; 64]> {
        let mut keypair = Zeroizing::new([0u8; 64]);
        let (secret, public) = keypair.split_at_mut(32);
        secret.copy_from_slice(self.seed.as_ref());
        public.copy_from_slice(&self.public);
        keypair
    }

    pub fn key_material(&self) -> KeyMaterial {
        KeyMaterial::new(KeyKind::Ed25519Seed, Zeroizing::new(self.seed.to_vec()))
    }

    /// RFC 8032 signing is deterministic, so a message always gets the same signature.
    pub fn sign(&self, message: &[u8]) -> [u8; 64] {
        SigningKey::from_bytes(&self.seed).sign(message).to_bytes()
    }
}

impl fmt::Debug for Ed25519Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Ed25519Key({}, <redacted>)", hex::encode(self.public))
    }
}
