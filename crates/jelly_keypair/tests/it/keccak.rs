use jelly_core::KeyError;
use jelly_keypair::{Secp256k1Key, keccak_address_bytes, keccak256};

#[test]
fn the_empty_input_hashes_to_the_published_keccak_digest() {
    assert_eq!(
        hex::encode(keccak256(b"")),
        "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
    );
}

/// The EIP-155 worked example signs with 32 bytes of 0x46 from this sender.
#[test]
fn the_address_from_a_public_key_is_the_eip_155_examples_sender() {
    let key = Secp256k1Key::from_secret_bytes(&[0x46; 32]).expect("a secret key");
    let sender = keccak_address_bytes(&key.public_uncompressed()).expect("an address");
    assert_eq!(hex::encode(sender), "9d8a62f656a8d1615c1294fd71e9cfb3e4855a4f");
}

#[test]
fn every_public_key_encoding_yields_the_same_address() {
    let key = Secp256k1Key::from_secret_bytes(&[0x46; 32]).expect("a secret key");
    let uncompressed = key.public_uncompressed();
    let from_compressed = keccak_address_bytes(&key.public_compressed()).expect("an address");
    let from_uncompressed = keccak_address_bytes(&uncompressed).expect("an address");
    let from_xy = keccak_address_bytes(&uncompressed[1..]).expect("an address");
    assert_eq!(from_compressed, from_uncompressed);
    assert_eq!(from_compressed, from_xy);
}

#[test]
fn a_hybrid_encoding_or_another_length_is_not_a_public_key() {
    assert!(matches!(keccak_address_bytes(&[0u8; 20]), Err(KeyError::InvalidPublicKey)));

    let key = Secp256k1Key::from_secret_bytes(&[0x46; 32]).expect("a secret key");
    let mut hybrid = key.public_uncompressed();
    hybrid[0] = 0x06;
    assert!(
        matches!(keccak_address_bytes(&hybrid), Err(KeyError::InvalidPublicKey)),
        "a hybrid encoding is not an uncompressed key"
    );
}
