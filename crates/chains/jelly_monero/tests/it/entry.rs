use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, KeyMaterial, ScriptKind,
    test_util::{MARKER, MARKER_PHRASE, assert_redacted},
};
use jelly_keypair::{PathTemplate, wif_encode};
use jelly_monero::{ENTRY, MoneroKeys, SPEC};
use zeroize::Zeroizing;

use crate::common::hd_account;

const CHAIN: ChainId = ChainId::Monero;
const MONERO: &str = "44cWztNFdAqNnycvZbUoj44vsbAEmKnx9aNgkjHdjtMsBrSeKiY8J4s2raH7EMawA2Fwo9utaRTV7Aw8EcTMNMxhH4YtKdH";

#[test]
fn the_entry_serves_its_own_spec_codec_and_endpoints() {
    assert_eq!(SPEC.chain, CHAIN);
    assert_eq!(ENTRY.spec(), &SPEC);
    assert_eq!(ENTRY.codec().chain(), CHAIN);
    assert_eq!(SPEC.transports.first(), Some(&SPEC.primary_transport));
    assert!(ENTRY.default_endpoints().contains("\"chain\": \"monero\""));
    assert_eq!(
        PathTemplate::parse(SPEC.default_path).unwrap().path_at(1).unwrap().to_string(),
        "44'/128'/1'"
    );
    assert!(matches!(SPEC.resolve_script(None, None), Ok(None)));
    assert!(matches!(
        SPEC.resolve_script(None, Some(ScriptKind::P2pkh)),
        Err(KeyError::ScriptNotApplicable { chain: ChainId::Monero })
    ));
    assert!(matches!(
        ENTRY.address(&hd_account(0).unwrap(), Some(ScriptKind::P2pkh)),
        Err(KeyError::ScriptNotApplicable { chain: ChainId::Monero })
    ));
}

/// The scanner watches only the wallet's own account, so Monero holds no standalone
/// keys, and naming its formats would send a user pasting one elsewhere to a chain that
/// cannot take it.
#[test]
fn monero_reads_and_recognizes_no_key() {
    let account = hd_account(0).unwrap();
    let hex_key = ENTRY.export_key(&account).unwrap();
    let words = ENTRY.account_seed_words(&account).unwrap();
    let wif = wif_encode(&[7; 32], 0x80);
    for input in [hex_key.as_str(), words.as_str(), wif.as_str(), MARKER, ""] {
        assert!(matches!(ENTRY.import_key(input, None), Err(KeyError::UnknownFormat)), "{input:?}");
        assert_eq!(ENTRY.recognize_key(input), None, "{input:?}");
    }
}

#[test]
fn hd_subaddresses_and_their_keys_agree() {
    let account = hd_account(0).unwrap();
    let keys = account
        .with_bytes(|bytes| MoneroKeys::from_spend_bytes(&<[u8; 32]>::try_from(bytes).unwrap()));
    assert_ne!(
        ENTRY.address(&account, None).unwrap(),
        ENTRY.address(&hd_account(1).unwrap(), None).unwrap()
    );
    for (major, minor) in [(0u32, 0u32), (0, 1), (1, 0), (2, 5)] {
        let address = ENTRY.subaddress(&account, major, minor).unwrap();
        assert_eq!(address, keys.address(major, minor).unwrap());
        assert_eq!(ENTRY.validate_address(&address).unwrap().value(), address);
        let key = ENTRY.subaddress_key(&account, major, minor).unwrap();
        assert_eq!(key.kind(), KeyKind::MoneroSpend);
        let exported = ENTRY.export_key(&key).unwrap();
        key.with_bytes(|bytes| assert_eq!(exported.as_str(), hex::encode(bytes)));
        keys.key_material_at(major, minor).with_bytes(|bytes| {
            assert_eq!(exported.as_str(), hex::encode(bytes));
        });
    }
}

#[test]
fn a_monero_address_has_no_script_pubkey() {
    let address = ENTRY.address(&hd_account(0).unwrap(), None).unwrap();
    assert_eq!(ENTRY.codec().script_pubkey(&address), Err(AddressError::NoScript { chain: CHAIN }));
    assert_eq!(ENTRY.codec().script_kind(&address), None);
}

#[test]
fn payment_uris_yield_the_address_they_pay() {
    for value in [
        format!("monero:{MONERO}?tx_amount=1&recipient_name=Shop"),
        format!("MONERO://{MONERO}"),
        format!("  {MONERO}\n"),
        String::from(MONERO),
    ] {
        assert_eq!(ENTRY.validate_address(&value).unwrap().value(), MONERO, "{value}");
    }
}

#[test]
fn payment_uris_refuse_what_jelly_cannot_send() {
    let unsupported = AddressError::UnsupportedRequest { chain: CHAIN };
    for value in [
        format!("monero:{MONERO}?tx_payment_id=0123456789abcdef"),
        format!("monero:{MONERO}?tx_amount=1&tx_payment_id=0123456789abcdef"),
        format!("monero:{MONERO}?tx_payment%5Fid=0123456789abcdef"),
        String::from("monero:?tx_amount=1"),
    ] {
        assert_eq!(ENTRY.validate_address(&value), Err(unsupported), "{value}");
    }
    for value in ["", "   ", "not an address", &format!("bitcoin:{MONERO}")] {
        ENTRY.validate_address(value).unwrap_err();
    }
}

#[test]
fn key_material_of_another_kind_is_refused() {
    let other = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(vec![7; 32]));
    let wrong_kind = |result: Result<_, KeyError>| {
        matches!(result, Err(KeyError::WrongKeyKind { chain: ChainId::Monero }))
    };
    assert!(wrong_kind(ENTRY.address(&other, None).map(drop)));
    assert!(wrong_kind(ENTRY.export_key(&other).map(drop)));
    assert!(wrong_kind(ENTRY.subaddress(&other, 0, 1).map(drop)));
    assert!(wrong_kind(ENTRY.subaddress_key(&other, 0, 1).map(drop)));
    assert!(wrong_kind(ENTRY.account_seed_words(&other).map(drop)));
}

#[test]
fn a_rejected_address_never_appears_in_its_error() {
    let seed = format!("{MARKER_PHRASE} qqqqsecret");
    for input in [MARKER, MARKER_PHRASE, seed.as_str()] {
        let refused = ENTRY.validate_address(input).unwrap_err();
        assert_redacted("validate_address", input, &KeyError::from(refused));
    }
}
