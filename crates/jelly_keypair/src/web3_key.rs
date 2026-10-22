//! The two forms an Ethereum-family wallet holds a secp256k1 key in: a Web3 Secret
//! Storage v3 document and the bare 32-byte scalar in hex.

use jelly_core::{ChainId, ImportedKey, KeyError, KeyKind, KeyMaterial, SecretText};
use zeroize::Zeroizing;

use crate::keystore::{decrypt_keystore_v3, is_keystore_v3};
use crate::secp256k1::Secp256k1Key;
use crate::secret_hex::{HEX_KEY_FORMAT, decode_secret_hex, encode_secret_hex, is_secret_hex};

/// The display name a [`KeyError::WrongChain`] gives a Web3 Secret Storage document.
const KEYSTORE_FORMAT: &str = "keystore file";

/// Names a keystore document or a 32-byte hex key from its shape, without decoding or
/// decrypting the secret.
pub fn recognize_web3_key(input: &str) -> Option<&'static str> {
    if is_keystore_v3(input) {
        return Some(KEYSTORE_FORMAT);
    }
    is_secret_hex(input).then_some(HEX_KEY_FORMAT)
}

/// Imports a Web3 Secret Storage v3 document or a 32-byte hex key, with or without its
/// `0x` prefix.
pub fn import_web3_key(
    input: &str,
    password: Option<&SecretText>,
) -> Result<ImportedKey, KeyError> {
    let trimmed = input.trim();
    let key = if is_keystore_v3(trimmed) {
        decrypt_keystore_v3(trimmed, password.ok_or(KeyError::PasswordRequired)?)?
    } else {
        let secret = decode_secret_hex(trimmed).ok_or(KeyError::UnknownFormat)?;
        Secp256k1Key::from_secret_bytes(secret.as_slice())?
    };
    Ok(ImportedKey { material: key.key_material(), named_script: None })
}

/// The 32-byte secret as lowercase hex without a prefix, which every Ethereum-family
/// wallet imports. Material of another kind is refused as having no key on `chain`.
pub fn export_web3_key(key: &KeyMaterial, chain: ChainId) -> Result<Zeroizing<String>, KeyError> {
    if key.kind() != KeyKind::Secp256k1 {
        return Err(KeyError::WrongKeyKind { chain });
    }
    Ok(Secp256k1Key::from_key_material(key)?.with_secret_bytes(encode_secret_hex))
}
