use std::fmt;

use bitcoin::bip32::{self, ChildNumber};
use hmac::{Hmac, KeyInit as _, Mac as _};
use jelly_core::KeyError;
use sha2::Sha512;
use zeroize::Zeroizing;

use crate::ed25519::Ed25519Key;

/// SLIP-10's HMAC key for the ed25519 master node.
const ED25519_CURVE: &[u8] = b"ed25519 seed";

type HmacSha512 = Hmac<Sha512>;

/// A SLIP-10 node, a 32-byte key and a 32-byte chain code, both secret.
///
/// SLIP-10 defines only hardened children for ed25519. A normal component is an error
/// rather than silently hardened, because wallets disagree on what silent hardening
/// means and a quiet disagreement restores the wrong addresses.
pub(crate) struct Slip10Node {
    key: Zeroizing<[u8; 32]>,
    chain_code: Zeroizing<[u8; 32]>,
}

impl Slip10Node {
    /// `I = HMAC-SHA512("ed25519 seed", seed)`. Every `IL` is a valid ed25519
    /// seed, so unlike BIP-32 there is no retry loop.
    pub(crate) fn from_seed_bytes(seed: &[u8]) -> Result<Self, KeyError> {
        let mut mac = HmacSha512::new_from_slice(ED25519_CURVE)
            .map_err(|_| KeyError::internal("hmac key length"))?;
        mac.update(seed);
        Ok(Self::split(&Zeroizing::new(mac.finalize().into_bytes().into())))
    }

    fn split(raw: &Zeroizing<[u8; 64]>) -> Self {
        let (key_half, chain_code_half) = raw.split_at(32);
        let mut key = Zeroizing::new([0u8; 32]);
        let mut chain_code = Zeroizing::new([0u8; 32]);
        key.copy_from_slice(key_half);
        chain_code.copy_from_slice(chain_code_half);
        Self { key, chain_code }
    }

    /// One hardened child, `I = HMAC-SHA512(c, 0x00 || k || ser32(i'))`.
    fn child(&self, child: ChildNumber) -> Result<Self, KeyError> {
        let mut mac = HmacSha512::new_from_slice(self.chain_code.as_ref())
            .map_err(|_| KeyError::internal("hmac key length"))?;
        mac.update(&[0u8]);
        mac.update(self.key.as_ref());
        mac.update(&u32::from(child).to_be_bytes());
        Ok(Self::split(&Zeroizing::new(mac.finalize().into_bytes().into())))
    }

    pub(crate) fn derive(self, path: &bip32::DerivationPath) -> Result<Self, KeyError> {
        let mut node = self;
        for (component, child) in path.into_iter().enumerate() {
            if !child.is_hardened() {
                return Err(KeyError::PathNotHardened { component });
            }
            node = node.child(*child)?;
        }
        Ok(node)
    }

    /// An ed25519 seed for the RFC 8032 expansion, or a Monero spend pre-image. It is not
    /// a scalar and must not be clamped or reduced here.
    pub(crate) fn with_key_bytes<R>(&self, read: impl FnOnce(&[u8; 32]) -> R) -> R {
        read(&self.key)
    }

    pub(crate) fn to_ed25519_key(&self) -> Ed25519Key {
        Ed25519Key::from_seed_bytes(&self.key)
    }
}

impl fmt::Debug for Slip10Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Slip10Node(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::fixtures::path;

    /// The ed25519 test vectors, verbatim from slip-0010.md.
    const SLIP10_JSON: &str = include_str!("../tests/vectors/slip10_ed25519.json");

    const VECTOR_1_SEED: &str = "000102030405060708090a0b0c0d0e0f";

    #[derive(Deserialize)]
    struct Slip10Vector {
        seed: String,
        chains: Vec<Slip10Step>,
    }

    #[derive(Deserialize)]
    struct Slip10Step {
        path: String,
        chain_code: String,
        private: String,
        public: String,
    }

    fn master(seed_hex: &str) -> Slip10Node {
        let seed = hex::decode(seed_hex).expect("the vector seed is hex");
        Slip10Node::from_seed_bytes(&seed).expect("master node")
    }

    #[test]
    fn derivation_matches_the_published_slip10_ed25519_vectors() {
        let vectors: Vec<Slip10Vector> =
            serde_json::from_str(SLIP10_JSON).expect("the vector file parses");
        assert_eq!(vectors.len(), 2);
        for vector in &vectors {
            for step in &vector.chains {
                let node = master(&vector.seed).derive(&path(&step.path)).expect("derive");
                assert_eq!(
                    node.with_key_bytes(|key| hex::encode(key)),
                    step.private,
                    "key at {}",
                    step.path
                );
                assert_eq!(
                    hex::encode(node.chain_code.as_slice()),
                    step.chain_code,
                    "chain code at {}",
                    step.path
                );
                // SLIP-10 prefixes an ed25519 public key with 0x00.
                assert_eq!(
                    format!("00{}", hex::encode(node.to_ed25519_key().public_bytes())),
                    step.public,
                    "public key at {}",
                    step.path
                );
            }
        }
    }

    #[test]
    fn a_normal_component_is_refused_at_its_position() {
        assert!(matches!(
            master(VECTOR_1_SEED).derive(&path("m/0'/1")),
            Err(KeyError::PathNotHardened { component: 1 })
        ));
    }

    #[test]
    fn a_slip10_node_redacts_its_debug() {
        let node = master(VECTOR_1_SEED);
        let debug = format!("{node:?}");
        assert!(debug.contains("redacted"));
        node.with_key_bytes(|key| assert!(!debug.contains(&hex::encode(key))));
        assert!(!debug.contains(&hex::encode(node.chain_code.as_slice())));
    }
}
