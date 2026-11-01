use std::error::Error;

use jelly_keypair::Secp256k1Key;

/// BIP-84 publishes the WIF for `m/84'/0'/0'/0/0` of the all-abandon phrase.
pub(crate) const BIP84_WIF: &str = "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d";

pub(crate) fn secp256k1_key(hex_secret: &str) -> Result<Secp256k1Key, Box<dyn Error>> {
    let mut secret = [0u8; 32];
    hex::decode_to_slice(hex_secret, &mut secret)?;
    Ok(Secp256k1Key::from_secret_bytes(&secret)?)
}
