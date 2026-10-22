//! Fixtures for other crates' tests. Only `[dev-dependencies]` enable the `test-util`
//! feature.

use jelly_core::{KeyError, KeyKind, KeyMaterial};
use zeroize::Zeroizing;

use crate::keychain::HdKeychain;
use crate::path::parse_path;
use crate::seed::Seed;
use crate::wif;

pub const TEST_PHRASE: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

/// The BIP-39 seed of the all-abandon phrase with no passphrase, from the published
/// BIP-39 vectors, so its keys, xpubs, and addresses are all documented values.
const TEST_SEED: [u8; Seed::LEN] = [
    0x5e, 0xb0, 0x0b, 0xbd, 0xdc, 0xf0, 0x69, 0x08, 0x48, 0x89, 0xa8, 0xab, 0x91, 0x55, 0x56, 0x81,
    0x65, 0xf5, 0xc4, 0x53, 0xcc, 0xb8, 0x5e, 0x70, 0x81, 0x1a, 0xae, 0xd6, 0xf6, 0xda, 0x5f, 0xc1,
    0x9a, 0x5a, 0xc4, 0x0b, 0x38, 0x9c, 0xd3, 0x70, 0xd0, 0x86, 0x20, 0x6d, 0xec, 0x8a, 0xa6, 0xc4,
    0x3d, 0xae, 0xa6, 0x69, 0x0f, 0x20, 0xad, 0x3d, 0x8d, 0x48, 0xb2, 0xd2, 0xce, 0x9e, 0x38, 0xe4,
];

/// The keychain of the all-abandon phrase with no passphrase.
pub fn test_keychain() -> HdKeychain {
    HdKeychain::from_seed(Seed::from_boxed(Box::new(TEST_SEED)))
}

/// The secp256k1 key of the all-abandon phrase at `path`.
pub fn key_at(path: &str) -> Result<KeyMaterial, KeyError> {
    test_keychain().key_material_at(KeyKind::Secp256k1, &parse_path(path)?)
}

impl Seed {
    /// Copies a seed a test vector names, to build a keychain without a phrase.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, KeyError> {
        if bytes.len() != Self::LEN {
            return Err(KeyError::InvalidSeedLength { got: bytes.len() });
        }
        let mut boxed: Box<[u8; Self::LEN]> = Box::new([0u8; Self::LEN]);
        boxed.copy_from_slice(bytes);
        Ok(Self::from_boxed(boxed))
    }
}

/// Base58check over `version || key` without the compression suffix, which a wallet
/// writes for a key whose funds sit at its uncompressed public key's address.
pub fn uncompressed_wif(key: &[u8; 32], version: u8) -> Zeroizing<String> {
    wif::encode(key, version, &[])
}
