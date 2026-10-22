use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, ScriptKind,
    test_util::{MARKER_PHRASE, assert_rejected_without_leak, key_material_of},
};
use jelly_dogecoin::{ENTRY, SPEC};
use jelly_utxo::test_util::address_at;

use crate::common::vectors;

#[test]
fn each_derived_vector_path_gives_its_recorded_address() {
    let suite = vectors().unwrap();
    assert_eq!(suite.derived.len(), 3);
    for row in &suite.derived {
        assert_eq!(address_at(ENTRY, &row.path, row.script).unwrap(), row.address, "{}", row.path);
    }
}

#[test]
fn each_raw_vector_key_gives_its_recorded_address() {
    for row in &vectors().unwrap().raw {
        let key = key_material_of(KeyKind::Secp256k1, hex::decode(&row.secret).unwrap());
        assert_eq!(ENTRY.address(&key, row.script).unwrap(), row.address);
    }
}

#[test]
fn each_script_vector_gives_its_recorded_script_and_kind() {
    let suite = vectors().unwrap();
    assert_eq!(suite.scripts.len(), 2);
    let codec = ENTRY.codec();
    assert_eq!(codec.chain(), ChainId::Dogecoin);
    for row in &suite.scripts {
        assert_eq!(
            hex::encode(codec.script_pubkey(&row.address).unwrap()),
            row.script_pubkey,
            "{}",
            row.address
        );
        assert_eq!(codec.script_kind(&row.address), row.script_kind, "{}", row.address);
        assert_eq!(ENTRY.validate_address(&row.address).unwrap().value(), row.address);
    }
}

#[test]
fn valid_addresses_canonicalise_to_themselves() {
    for case in &vectors().unwrap().valid {
        let address = ENTRY.validate_address(case).unwrap();
        assert_eq!(address.value(), case);
        assert_eq!(address.chain(), ChainId::Dogecoin);
        // Surrounding whitespace is the caller's paste, not part of the value.
        let padded = format!("  {case}  ");
        assert_eq!(ENTRY.validate_address(&padded).unwrap().value(), case);
    }
}

#[test]
fn invalid_addresses_are_refused() {
    for case in &vectors().unwrap().invalid {
        assert!(ENTRY.validate_address(&case.address).is_err(), "{}: {}", case.address, case.why);
    }
    ENTRY.validate_address("").unwrap_err();
    ENTRY.validate_address("   ").unwrap_err();
}

#[test]
fn the_codec_reports_the_flavour_a_path_derived() {
    let address = address_at(ENTRY, "m/44'/3'/0'/0/0", None).unwrap();
    assert_eq!(ENTRY.codec().script_kind(&address), Some(ScriptKind::P2pkh));
    assert!(!ENTRY.codec().script_pubkey(&address).unwrap().is_empty());
}

/// Dogecoin never activated segregated witness, so P2PKH is the only flavour and a
/// segwit path is refused rather than downgraded.
#[test]
fn p2pkh_is_the_only_flavour() {
    assert_eq!(SPEC.default_script, Some(ScriptKind::P2pkh));
    assert_eq!(
        address_at(ENTRY, "m/0/0", None).unwrap(),
        address_at(ENTRY, "m/0/0", Some(ScriptKind::P2pkh)).unwrap()
    );
    for (text, script) in [
        ("m/84'/3'/0'/0/0", ScriptKind::P2wpkh),
        ("m/49'/3'/0'/0/0", ScriptKind::P2shP2wpkh),
        ("m/86'/3'/0'/0/0", ScriptKind::P2tr),
    ] {
        assert!(
            matches!(
                address_at(ENTRY, text, None),
                Err(KeyError::ScriptUnsupported { chain: ChainId::Dogecoin, script: refused })
                    if refused == script
            ),
            "{text}"
        );
    }
}

#[test]
fn payment_uris_yield_the_address_they_pay() {
    for case in &vectors().unwrap().uris.accepted {
        assert_eq!(
            ENTRY.validate_address(&case.uri).unwrap().value(),
            case.address,
            "{}",
            case.uri
        );
    }
}

#[test]
fn payment_uris_refuse_what_jelly_cannot_send() {
    for case in &vectors().unwrap().uris.unsupported {
        assert_eq!(
            ENTRY.validate_address(&case.uri),
            Err(AddressError::UnsupportedRequest { chain: ChainId::Dogecoin }),
            "{}: {}",
            case.uri,
            case.why
        );
    }
}

#[test]
fn a_rejected_address_never_appears_in_its_error() {
    assert_rejected_without_leak(
        "validate_address",
        MARKER_PHRASE,
        ENTRY.validate_address(MARKER_PHRASE).map_err(KeyError::from),
    );
}
