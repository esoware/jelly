use jelly_core::{ChainId, KeyError, KeyKind, KeyMaterial};
use jelly_keypair::{
    decode_secret_hex, encode_secret_hex, export_web3_key, import_web3_key, is_secret_hex,
    recognize_web3_key, test_util::uncompressed_wif, wif_decode, wif_encode, wif_version,
};
use zeroize::Zeroizing;

use crate::common::BIP84_WIF;

const SECRET_HEX: &str = "4646464646464646464646464646464646464646464646464646464646464646";

#[test]
fn a_wif_round_trips_through_its_version_and_compression() {
    let decoded = wif_decode(BIP84_WIF).expect("a wif");
    assert_eq!(decoded.version, 0x80);
    assert!(decoded.is_compressed);
    let secret = Zeroizing::new(decoded.key.with_secret_bytes(|secret| *secret));
    assert_eq!(wif_encode(&secret, 0x80).as_str(), BIP84_WIF);
    let uncompressed = uncompressed_wif(&secret, 0xb0);
    let again = wif_decode(&uncompressed).expect("a wif");
    assert_eq!(again.version, 0xb0);
    assert!(!again.is_compressed);
    again.key.with_secret_bytes(|again| assert_eq!(again, &*secret));
}

#[test]
fn other_text_is_not_a_wif() {
    for text in ["", "abc", "0x11", &"1".repeat(51)] {
        assert!(matches!(wif_decode(text), Err(KeyError::UnknownFormat)), "{text}");
        assert_eq!(wif_version(text), None, "{text}");
    }
}

#[test]
fn a_wif_outside_the_curve_order_is_refused() {
    assert!(matches!(wif_decode(&wif_encode(&[0xff; 32], 0x80)), Err(KeyError::InvalidSecretKey)));
}

#[test]
fn the_wif_version_is_read_from_the_shape_without_the_key() {
    assert_eq!(wif_version(BIP84_WIF), Some(0x80));
    assert_eq!(wif_version(&format!(" {BIP84_WIF} ")), Some(0x80));
    assert_eq!(wif_version(&uncompressed_wif(&[7; 32], 0xb0)), Some(0xb0));
    assert_eq!(wif_version(&wif_encode(&[0xff; 32], 0x9e)), Some(0x9e));
}

#[test]
fn hex_round_trips_with_or_without_a_prefix() {
    let encoded = encode_secret_hex(&[0xabu8; 32]);
    assert_eq!(encoded.len(), 64);
    let prefixed = format!(" 0X{} ", encoded.as_str());
    assert!(is_secret_hex(&encoded));
    assert!(is_secret_hex(&prefixed));
    assert_eq!(*decode_secret_hex(&encoded).expect("hex"), [0xab; 32]);
    assert_eq!(*decode_secret_hex(&prefixed).expect("hex"), [0xab; 32]);
}

#[test]
fn other_lengths_and_characters_are_not_hex_keys() {
    for input in ["ab".repeat(31), "ab".repeat(33), "zz".repeat(32)] {
        assert!(!is_secret_hex(&input), "{input}");
        assert!(decode_secret_hex(&input).is_none(), "{input}");
    }
}

#[test]
fn a_hex_key_is_named_imported_and_exported_back() {
    assert_eq!(recognize_web3_key(SECRET_HEX), Some("32-byte hex key"));
    let imported = import_web3_key(&format!(" 0x{SECRET_HEX} "), None).expect("import");
    assert_eq!(imported.named_script, None);
    let exported = export_web3_key(&imported.material, ChainId::Ethereum).expect("export");
    assert_eq!(exported.as_str(), SECRET_HEX);
}

#[test]
fn a_keystore_is_named_by_its_shape_and_needs_its_password() {
    let document = r#"{"crypto":{},"version":3}"#;
    assert_eq!(recognize_web3_key(document), Some("keystore file"));
    assert!(matches!(import_web3_key(document, None), Err(KeyError::PasswordRequired)));
}

#[test]
fn other_text_and_other_curves_are_refused() {
    assert_eq!(recognize_web3_key("0x1234"), None);
    assert!(matches!(import_web3_key("0x1234", None), Err(KeyError::UnknownFormat)));
    let ed25519 = KeyMaterial::new(KeyKind::Ed25519Seed, Zeroizing::new(vec![7; 32]));
    assert!(matches!(
        export_web3_key(&ed25519, ChainId::Tron),
        Err(KeyError::WrongKeyKind { chain: ChainId::Tron })
    ));
}
