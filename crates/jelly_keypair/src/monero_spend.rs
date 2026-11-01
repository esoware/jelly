use curve25519_dalek::Scalar;
use jelly_core::{KeyKind, KeyMaterial};
use zeroize::Zeroizing;

/// Reduces a SLIP-10 key or raw entropy to a spend scalar, the way Monero wallets turn
/// their seed into a spend key.
pub(crate) fn monero_spend_material(bytes: &[u8; 32]) -> KeyMaterial {
    let scalar = Zeroizing::new(Scalar::from_bytes_mod_order(*bytes));
    KeyMaterial::new(KeyKind::MoneroSpend, Zeroizing::new(scalar.to_bytes().to_vec()))
}
