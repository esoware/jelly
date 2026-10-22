use jelly_core::KeyError;
use secp256k1::PublicKey;
use sha3::{Digest as _, Keccak256};

/// Keccak-256 with the original padding, not SHA3-256.
pub fn keccak256(data: &[u8]) -> [u8; 32] {
    Keccak256::digest(data).into()
}

/// The last twenty bytes of the Keccak-256 of a secp256k1 point's coordinates, which
/// is the address body an EVM account and a TRON account share.
///
/// Accepts a 33-byte compressed key, a 65-byte uncompressed key, or the bare 64-byte
/// coordinate pair.
pub fn keccak_address_bytes(pubkey: &[u8]) -> Result<[u8; 20], KeyError> {
    let digest = keccak256(&uncompressed_xy(pubkey)?);
    let mut tail = [0u8; 20];
    tail.copy_from_slice(&digest[12..]);
    Ok(tail)
}

fn uncompressed_xy(pubkey: &[u8]) -> Result<[u8; 64], KeyError> {
    match pubkey.len() {
        64 => pubkey.try_into().map_err(|_| KeyError::InvalidPublicKey),
        // The prefix is checked here rather than by `PublicKey::from_slice`, which would
        // also accept the hybrid `0x06` and `0x07` encodings.
        65 => match pubkey.split_first() {
            Some((0x04, xy)) => xy.try_into().map_err(|_| KeyError::InvalidPublicKey),
            _ => Err(KeyError::InvalidPublicKey),
        },
        33 => {
            let [_, xy @ ..] = PublicKey::from_slice(pubkey)
                .map_err(|_| KeyError::InvalidPublicKey)?
                .serialize_uncompressed();
            Ok(xy)
        }
        _ => Err(KeyError::InvalidPublicKey),
    }
}
