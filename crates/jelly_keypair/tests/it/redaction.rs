use jelly_core::test_util::{MARKER, assert_rejected_without_leak};
use jelly_keypair::{Secp256k1Key, Seed, parse_path, test_util::test_keychain, wif_decode};

use crate::common::BIP84_WIF;

#[test]
fn a_rejected_private_key_or_seed_never_appears_in_its_error() {
    let hex_key = format!("0x{}", "ff".repeat(32));
    assert_rejected_without_leak(
        "Secp256k1Key::from_secret_bytes",
        &hex_key,
        Secp256k1Key::from_secret_bytes(&[0xffu8; 32]),
    );
    assert_rejected_without_leak("Seed::from_bytes", MARKER, Seed::from_bytes(MARKER.as_bytes()));
}

#[test]
fn a_rejected_path_never_appears_in_its_error() {
    let path = format!("m/44'/{MARKER}/0'");
    assert_rejected_without_leak("parse_path", &path, parse_path(&path));
}

#[test]
fn a_seed_redacts_its_debug() {
    let seed = Seed::from_boxed(Box::new([7u8; Seed::LEN]));
    let debug = format!("{seed:?}");
    assert!(debug.contains("redacted"));
    seed.with_bytes(|bytes| assert!(!debug.contains(&hex::encode(bytes))));
}

#[test]
fn a_keychain_redacts_its_debug() {
    let keychain = test_keychain();
    let debug = format!("{keychain:?}");
    assert!(debug.contains("redacted"));
    keychain.seed().with_bytes(|bytes| assert!(!debug.contains(&hex::encode(bytes))));
}

#[test]
fn a_secp256k1_key_redacts_its_debug() {
    let key =
        test_keychain().secp256k1_at(&parse_path("m/44'/60'/0'/0/0").unwrap()).expect("a key");
    let debug = format!("{key:?}");
    assert!(debug.contains("redacted"));
    key.with_secret_bytes(|bytes| assert!(!debug.contains(&hex::encode(bytes))));
}

#[test]
fn an_ed25519_key_redacts_its_debug() {
    let key = test_keychain().ed25519_at(&parse_path("m/44'/501'/0'/0'").unwrap()).expect("a key");
    let debug = format!("{key:?}");
    assert!(debug.contains("redacted"));
    key.with_secret_bytes(|seed| assert!(!debug.contains(&hex::encode(seed))));
}

#[test]
fn a_wif_redacts_its_debug() {
    let decoded = wif_decode(BIP84_WIF).expect("a wif");
    let debug = format!("{decoded:?}");
    decoded.key.with_secret_bytes(|bytes| assert!(!debug.contains(&hex::encode(bytes))));
}
