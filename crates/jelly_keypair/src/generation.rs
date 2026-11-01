use jelly_core::{KeyError, KeyKind, KeyMaterial, random_bytes};

use crate::ed25519::Ed25519Key;
use crate::monero_spend::monero_spend_material;
use crate::secp256k1::Secp256k1Key;

/// A fresh independent key of `kind` from OS entropy.
pub fn generate_key(kind: KeyKind) -> Result<KeyMaterial, KeyError> {
    match kind {
        // A draw outside the curve order is retried, which happens with probability below
        // 2^-127.
        KeyKind::Secp256k1 => loop {
            let secret = random_bytes::<32>()?;
            if let Ok(key) = Secp256k1Key::from_secret_bytes(&secret) {
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
