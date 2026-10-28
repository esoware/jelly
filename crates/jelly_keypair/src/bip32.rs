use std::fmt;

use bitcoin::NetworkKind;
use bitcoin::bip32::{self, Xpriv, Xpub};
use jelly_core::KeyError;
use zeroize::Zeroizing;

use crate::secp256k1::{Secp256k1Key, signing_context};

// BIP-32 fixes the master seed at 128 to 512 bits.
const MIN_SEED_BYTES: usize = 16;
const MAX_SEED_BYTES: usize = 64;

/// The wrapped secret is erased on drop, and [`Bip32Node::derive`] walks one component
/// at a time so every intermediate node is erased as soon as its child exists.
pub(crate) struct Bip32Node {
    inner: Xpriv,
}

impl Bip32Node {
    /// Accepts the 16 to 64 byte range BIP-32 fixes. We check the length, since the
    /// library doesn't.
    pub(crate) fn from_seed_bytes(bytes: &[u8]) -> Result<Self, KeyError> {
        if !(MIN_SEED_BYTES..=MAX_SEED_BYTES).contains(&bytes.len()) {
            return Err(KeyError::InvalidSeedLength { got: bytes.len() });
        }
        let inner =
            Xpriv::new_master(NetworkKind::Main, bytes).map_err(|_| KeyError::Derivation)?;
        Ok(Self { inner })
    }

    pub(crate) fn derive(self, path: &bip32::DerivationPath) -> Result<Self, KeyError> {
        let context = signing_context();
        let mut node = self;
        for child in path {
            let next = node.inner.derive_priv(context, child).map_err(|_| KeyError::Derivation)?;
            node = Self { inner: next };
        }
        Ok(node)
    }

    pub(crate) fn to_secp256k1_key(&self) -> Result<Secp256k1Key, KeyError> {
        let secret = Zeroizing::new(self.inner.private_key.secret_bytes());
        Secp256k1Key::from_secret_bytes(secret.as_ref())
    }

    /// Base58 `xpub`, for watch-only export.
    ///
    /// An xpub plus any non-hardened child private key recovers the parent xprv, which is
    /// why account levels and above are hardened.
    pub(crate) fn to_xpub_string(&self) -> String {
        Xpub::from_priv(signing_context(), &self.inner).to_string()
    }

    #[cfg(test)]
    fn to_xprv_string(&self) -> Zeroizing<String> {
        Zeroizing::new(self.inner.to_string())
    }

    /// Secret-adjacent. With the public key, a chain code plus one non-hardened child
    /// private key recovers the parent private key.
    #[cfg(test)]
    fn chain_code(&self) -> [u8; 32] {
        self.inner.chain_code.to_bytes()
    }
}

impl fmt::Debug for Bip32Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Bip32Node(depth={}, <redacted>)", self.inner.depth)
    }
}

impl Drop for Bip32Node {
    fn drop(&mut self) {
        self.inner.private_key.non_secure_erase();
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::fixtures::path;

    /// BIP-32 test vectors 1 to 4, verbatim from bip-0032.mediawiki.
    const BIP32_JSON: &str = include_str!("../tests/vectors/bip32.json");

    const VECTOR_1_SEED: &str = "000102030405060708090a0b0c0d0e0f";

    #[derive(Deserialize)]
    struct Bip32Vector {
        seed: String,
        chains: Vec<Bip32Step>,
    }

    #[derive(Deserialize)]
    struct Bip32Step {
        path: String,
        xpub: String,
        xprv: String,
    }

    fn master(seed_hex: &str) -> Bip32Node {
        let seed = hex::decode(seed_hex).expect("the vector seed is hex");
        Bip32Node::from_seed_bytes(&seed).expect("master node")
    }

    #[test]
    fn derivation_matches_the_published_bip32_vectors() {
        let vectors: Vec<Bip32Vector> =
            serde_json::from_str(BIP32_JSON).expect("the vector file parses");
        assert_eq!(vectors.len(), 4, "vectors 1 to 4 present");
        for vector in &vectors {
            for step in &vector.chains {
                let node = master(&vector.seed).derive(&path(&step.path)).expect("derive");
                assert_eq!(node.to_xprv_string().as_str(), step.xprv, "xprv at {}", step.path);
                assert_eq!(node.to_xpub_string(), step.xpub, "xpub at {}", step.path);
            }
        }
    }

    #[test]
    fn stepwise_derivation_equals_the_whole_path() {
        let full = master(VECTOR_1_SEED).derive(&path("m/0'/1/2'")).expect("derive");
        let step = master(VECTOR_1_SEED)
            .derive(&path("m/0'"))
            .expect("derive")
            .derive(&path("m/1"))
            .expect("derive")
            .derive(&path("m/2'"))
            .expect("derive");
        assert_eq!(full.to_xpub_string(), step.to_xpub_string());
    }

    #[test]
    fn a_seed_outside_the_bip32_range_is_refused() {
        assert!(matches!(
            Bip32Node::from_seed_bytes(&[0u8; 15]),
            Err(KeyError::InvalidSeedLength { got: 15 })
        ));
        assert!(matches!(
            Bip32Node::from_seed_bytes(&[0u8; 65]),
            Err(KeyError::InvalidSeedLength { got: 65 })
        ));
        Bip32Node::from_seed_bytes(&[0u8; 16]).unwrap();
        Bip32Node::from_seed_bytes(&[0u8; 64]).unwrap();
    }

    #[test]
    fn a_bip32_node_redacts_its_debug() {
        let node = master(VECTOR_1_SEED);
        let debug = format!("{node:?}");
        assert!(debug.contains("redacted"));
        assert!(!debug.contains(node.to_xprv_string().as_str()));
        assert!(!debug.contains(&hex::encode(node.chain_code())));
    }
}
