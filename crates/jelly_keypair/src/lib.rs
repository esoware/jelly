//! Curve keys, BIP-32 and SLIP-10 derivation, derivation paths, and the key file formats
//! more than one chain reads. Knows no chain.
//!
//! Secrets live in zeroizing storage with redacted `Debug`, and errors carry positions and
//! counts, never input fragments.

#![deny(clippy::float_arithmetic)]
mod bip32;
mod ed25519;
#[cfg(test)]
mod fixtures;
mod keccak;
mod keychain;
mod keystore;
mod path;
mod secp256k1;
mod secret_hex;
mod seed;
mod slip10;
#[cfg(feature = "test-util")]
pub mod test_util;
mod web3_key;
mod wif;

pub use crate::ed25519::Ed25519Key;
pub use crate::keccak::{keccak_address_bytes, keccak256};
pub use crate::keychain::{HdKeychain, generate_key};
pub use crate::path::{
    PathTemplate, account_prefix, child_index, parse_path, purpose_of, stored_path,
};
pub use crate::secp256k1::{Secp256k1Key, signing_context, verification_context};
pub use crate::secret_hex::{HEX_KEY_FORMAT, decode_secret_hex, encode_secret_hex, is_secret_hex};
pub use crate::seed::Seed;
pub use crate::web3_key::{export_web3_key, import_web3_key, recognize_web3_key};
pub use crate::wif::{Wif, wif_decode, wif_encode, wif_version};
pub use bitcoin::bip32::DerivationPath as Bip32Path;
