//! Fixtures the unit tests share.

use jelly_keypair::Secp256k1Key;

use crate::address::pubkey_address;

pub(crate) const OWNER: &str = "t1Wg9uPPAfwhBWeRjtDPa5ZHNzyBx9rJVKY";
pub(crate) const RECIPIENT: &str = "t1VmmGiyjVNeCjxDZzg7vZmd99WyzVby9yC";
pub(crate) const SCRIPT_HASH: &str = "t3RD6RFKhWSotNbPEY4Vw7Ku9QCfKkzrbBL";

/// The key whose secret is 32 bytes of `seed`.
pub(crate) fn key(seed: u8) -> Secp256k1Key {
    Secp256k1Key::from_secret_bytes(&[seed; 32]).unwrap()
}

/// The P2PKH address of [`key`] at `seed`.
pub(crate) fn key_address(seed: u8) -> String {
    pubkey_address(&key(seed).public_compressed()).unwrap()
}
