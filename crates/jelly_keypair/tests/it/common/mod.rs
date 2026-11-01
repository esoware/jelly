use std::error::Error;

use jelly_keypair::Secp256k1Key;

pub(crate) fn secp256k1_key(hex_secret: &str) -> Result<Secp256k1Key, Box<dyn Error>> {
    let mut secret = [0u8; 32];
    hex::decode_to_slice(hex_secret, &mut secret)?;
    Ok(Secp256k1Key::from_secret_bytes(&secret)?)
}
