use bitcoin::base58;
use jelly_core::KeyError;
use zeroize::Zeroizing;

use crate::secp256k1::Secp256k1Key;

/// The byte after the key that marks its public key as compressed.
const COMPRESSED_SUFFIX: u8 = 0x01;

/// A decoded wallet import format key. The version byte names the network, and the
/// chain decides what it means.
#[derive(Debug)]
pub struct Wif {
    pub version: u8,
    pub key: Secp256k1Key,
    /// Whether the `0x01` suffix was present. We derive every address from the compressed
    /// public key, so an uncompressed WIF's funds are somewhere else.
    pub is_compressed: bool,
}

/// Base58check over `version || key || 0x01`, the compressed form every exported key
/// takes.
pub fn wif_encode(key: &[u8; 32], version: u8) -> Zeroizing<String> {
    encode(key, version, &[COMPRESSED_SUFFIX])
}

/// Base58check over `version || key || suffix`.
pub(crate) fn encode(key: &[u8; 32], version: u8, suffix: &[u8]) -> Zeroizing<String> {
    let mut payload = Zeroizing::new(Vec::with_capacity(34));
    payload.push(version);
    payload.extend_from_slice(key);
    payload.extend_from_slice(suffix);
    Zeroizing::new(base58::encode_check(&payload))
}

/// The version byte of text in the shape of a WIF, read without checking the key it
/// holds.
pub fn wif_version(input: &str) -> Option<u8> {
    let raw = Zeroizing::new(base58::decode_check(input.trim()).ok()?);
    let (version, _, _) = split(&raw)?;
    Some(version)
}

/// Reads any base58check WIF, whatever its version byte. Text in another shape is
/// [`KeyError::UnknownFormat`], and a scalar outside the curve order is refused here
/// rather than at signing time.
pub fn wif_decode(input: &str) -> Result<Wif, KeyError> {
    let raw =
        Zeroizing::new(base58::decode_check(input.trim()).map_err(|_| KeyError::UnknownFormat)?);
    let (version, key, is_compressed) = split(&raw).ok_or(KeyError::UnknownFormat)?;
    Ok(Wif { version, key: Secp256k1Key::from_secret_bytes(key)?, is_compressed })
}

/// The version byte, the key, and whether the compression suffix follows it.
fn split(raw: &[u8]) -> Option<(u8, &[u8], bool)> {
    let (&version, rest) = raw.split_first()?;
    match rest.len() {
        33 => match rest.split_last() {
            Some((&COMPRESSED_SUFFIX, key)) => Some((version, key, true)),
            _ => None,
        },
        32 => Some((version, rest, false)),
        _ => None,
    }
}
