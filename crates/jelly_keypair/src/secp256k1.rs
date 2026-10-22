use std::fmt;
use std::sync::OnceLock;

use jelly_core::{KeyError, KeyKind, KeyMaterial};
use secp256k1::{Message, PublicKey, Secp256k1, SecretKey, SignOnly, VerifyOnly};
use zeroize::Zeroizing;

/// The process's one signing context. No call site builds its own, so its setup happens
/// once.
pub fn signing_context() -> &'static Secp256k1<SignOnly> {
    static CONTEXT: OnceLock<Secp256k1<SignOnly>> = OnceLock::new();
    CONTEXT.get_or_init(Secp256k1::signing_only)
}

/// The process's verification context, needed for taproot output keys.
pub fn verification_context() -> &'static Secp256k1<VerifyOnly> {
    static CONTEXT: OnceLock<Secp256k1<VerifyOnly>> = OnceLock::new();
    CONTEXT.get_or_init(Secp256k1::verification_only)
}

/// `secret` is a valid scalar in `[1, n-1]`, wiped on drop, and `public` is its
/// basepoint multiple.
pub struct Secp256k1Key {
    secret: Zeroizing<[u8; 32]>,
    public: PublicKey,
}

impl Secp256k1Key {
    pub fn from_secret_bytes(bytes: &[u8]) -> Result<Self, KeyError> {
        let mut secret_key =
            SecretKey::from_slice(bytes).map_err(|_| KeyError::InvalidSecretKey)?;
        let public = PublicKey::from_secret_key(signing_context(), &secret_key);
        let secret = Zeroizing::new(secret_key.secret_bytes());
        secret_key.non_secure_erase();
        Ok(Self { secret, public })
    }

    /// The key `material` holds, refusing material of another kind.
    pub fn from_key_material(material: &KeyMaterial) -> Result<Self, KeyError> {
        if material.kind() != KeyKind::Secp256k1 {
            return Err(KeyError::InvalidSecretKey);
        }
        material.with_bytes(Self::from_secret_bytes)
    }

    pub fn with_secret_bytes<R>(&self, read: impl FnOnce(&[u8; 32]) -> R) -> R {
        read(&self.secret)
    }

    pub fn key_material(&self) -> KeyMaterial {
        KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(self.secret.to_vec()))
    }

    /// SEC1 compressed, 33 bytes.
    pub fn public_compressed(&self) -> [u8; 33] {
        self.public.serialize()
    }

    /// SEC1 uncompressed, 65 bytes with the leading `0x04`.
    pub fn public_uncompressed(&self) -> [u8; 65] {
        self.public.serialize_uncompressed()
    }

    /// The workspace's only recoverable-ECDSA entry point, returning `r || s || recid`
    /// with a raw recovery id in `0..=3`, the layout TRON signatures use.
    ///
    /// The nonce is RFC 6979 and `s` is low. Nothing here inspects or re-hashes the
    /// input, so the caller must know which domain produced the digest. EIP-155 and
    /// legacy Ethereum offset the id differently, and that belongs to the chain crate.
    pub fn sign_prehash_recoverable(&self, digest: &[u8; 32]) -> Result<[u8; 65], KeyError> {
        let signature = self.with_secret_key(|secret| {
            signing_context().sign_ecdsa_recoverable(&Message::from_digest(*digest), secret)
        })?;
        let (recovery_id, compact) = signature.serialize_compact();
        let recovery_id = u8::try_from(recovery_id.to_i32())
            .map_err(|_| KeyError::internal("recovery id out of range"))?;
        let mut recoverable = [0u8; 65];
        let [signature @ .., recovery] = &mut recoverable;
        *signature = compact;
        *recovery = recovery_id;
        Ok(recoverable)
    }

    pub fn sign_prehash_der(&self, digest: &[u8; 32]) -> Result<Vec<u8>, KeyError> {
        let signature = self.with_secret_key(|secret| {
            signing_context().sign_ecdsa(&Message::from_digest(*digest), secret)
        })?;
        Ok(signature.serialize_der().to_vec())
    }

    /// Runs `run` on the library key rebuilt from the stored scalar, then erases it.
    fn with_secret_key<R>(&self, run: impl FnOnce(&SecretKey) -> R) -> Result<R, KeyError> {
        let mut secret = SecretKey::from_slice(self.secret.as_ref())
            .map_err(|_| KeyError::internal("stored secp256k1 scalar"))?;
        let result = run(&secret);
        secret.non_secure_erase();
        Ok(result)
    }
}

impl fmt::Debug for Secp256k1Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Secp256k1Key({}, <redacted>)", hex::encode(self.public_compressed()))
    }
}
