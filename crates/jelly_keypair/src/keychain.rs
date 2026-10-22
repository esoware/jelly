use std::fmt;

use bitcoin::bip32;
use curve25519_dalek::Scalar;
use jelly_core::{KeyError, KeyKind, KeyMaterial, random_bytes};
use zeroize::Zeroizing;

use crate::bip32::Bip32Node;
use crate::ed25519::Ed25519Key;
use crate::secp256k1::Secp256k1Key;
use crate::seed::Seed;
use crate::slip10::Slip10Node;

/// A seed and the derivation rules over it. Holds no chain state, does no I/O, and every
/// method is a pure function of the seed and its arguments.
///
/// The session holds the expanded seed rather than the entropy and passphrase.
/// Re-deriving per operation would still keep both inputs alive, so it would narrow no
/// window while adding a PBKDF2 pass to every address.
pub struct HdKeychain {
    seed: Seed,
}

impl HdKeychain {
    pub const fn from_seed(seed: Seed) -> Self {
        Self { seed }
    }

    pub const fn seed(&self) -> &Seed {
        &self.seed
    }

    fn bip32_at(&self, path: &bip32::DerivationPath) -> Result<Bip32Node, KeyError> {
        self.seed.with_bytes(|bytes| Bip32Node::from_seed_bytes(bytes))?.derive(path)
    }

    fn slip10_at(&self, path: &bip32::DerivationPath) -> Result<Slip10Node, KeyError> {
        self.seed.with_bytes(|bytes| Slip10Node::from_seed_bytes(bytes))?.derive(path)
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
    /// Any path the curve can walk is accepted, including a custom depth or index,
    /// because a wallet that refuses unusual paths cannot restore keys made elsewhere.
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

impl fmt::Debug for HdKeychain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("HdKeychain(<redacted>)")
    }
}

/// A fresh independent key of `kind` from OS entropy.
pub fn generate_key(kind: KeyKind) -> Result<KeyMaterial, KeyError> {
    match kind {
        // A draw outside the curve order is retried, which happens with probability below
        // 2^-127.
        KeyKind::Secp256k1 => loop {
            let secret = random_bytes::<32>()?;
            if let Ok(key) = Secp256k1Key::from_secret_bytes(secret.as_ref()) {
                return Ok(key.key_material());
            }
        },
        KeyKind::Ed25519Seed => {
            let seed = random_bytes::<32>()?;
            Ok(Ed25519Key::from_seed_bytes(&seed).key_material())
        }
        KeyKind::MoneroSpend => {
            let entropy = random_bytes::<32>()?;
            Ok(monero_spend_material(&entropy))
        }
    }
}

/// Reduces a SLIP-10 key or raw entropy to the spend scalar, which is what every
/// deterministic Monero wallet does with its seed.
fn monero_spend_material(bytes: &[u8; 32]) -> KeyMaterial {
    let scalar = Zeroizing::new(Scalar::from_bytes_mod_order(*bytes));
    KeyMaterial::new(KeyKind::MoneroSpend, Zeroizing::new(scalar.to_bytes().to_vec()))
}
