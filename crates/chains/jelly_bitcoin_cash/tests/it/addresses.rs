use jelly_bitcoin_cash::{ENTRY, SPEC};
use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, ScriptKind,
    test_util::{MARKER_PHRASE, assert_rejected_without_leak, key_material_of},
};
use jelly_utxo::test_util::address_at;
use serde::Deserialize;

use crate::common::vectors;

/// A `CashAddr` with `b`, which base32 lacks, as its last character.
const OUTSIDE_ALPHABET: &str = "bitcoincash:qr6m7j9njldwwzlg9v7v53unlr4jkmx6eylep8ekgb";

const CASHADDR_JSON: &str = include_str!("../vectors/cashaddr.json");

#[derive(Deserialize)]
struct Translations {
    legacy: Vec<Translation>,
}

#[derive(Deserialize)]
struct Translation {
    legacy: String,
    cashaddr: String,
}

#[test]
fn each_derived_vector_path_gives_its_recorded_address() {
    let suite = vectors().unwrap();
    assert_eq!(suite.derived.len(), 4);
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
    assert_eq!(suite.scripts.len(), 8);
    let codec = ENTRY.codec();
    assert_eq!(codec.chain(), ChainId::BitcoinCash);
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
        assert_eq!(address.chain(), ChainId::BitcoinCash);
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
    for text in ["m/44'/145'/0'/0/0", "m/44'/0'/0'/0/0"] {
        let address = address_at(ENTRY, text, None).unwrap();
        assert_eq!(ENTRY.codec().script_kind(&address), Some(ScriptKind::P2pkh), "{text}");
        assert!(!ENTRY.codec().script_pubkey(&address).unwrap().is_empty());
    }
}

/// The specification's legacy translations, each a legacy address that pays the script
/// its `CashAddr` translation pays, which is written back as that translation.
#[test]
fn a_legacy_address_pays_the_script_of_its_published_cashaddr_translation() {
    let file: Translations = serde_json::from_str(CASHADDR_JSON).unwrap();
    assert_eq!(file.legacy.len(), 6);
    let codec = ENTRY.codec();
    for row in &file.legacy {
        let script_pubkey = codec.script_pubkey(&row.legacy).unwrap();
        assert_eq!(script_pubkey, codec.script_pubkey(&row.cashaddr).unwrap(), "{}", row.legacy);
        assert_eq!(
            codec.address_of(&script_pubkey).as_deref(),
            Some(row.cashaddr.as_str()),
            "{}",
            row.legacy
        );
    }
}

/// Bitcoin Cash has no segwit, so P2PKH is the only flavour and a segwit path is
/// refused rather than downgraded.
#[test]
fn p2pkh_is_the_only_flavour() {
    assert_eq!(SPEC.default_script, Some(ScriptKind::P2pkh));
    assert!(matches!(
        address_at(ENTRY, "m/84'/145'/0'/0/0", None),
        Err(KeyError::ScriptUnsupported {
            chain: ChainId::BitcoinCash,
            script: ScriptKind::P2wpkh
        })
    ));
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
    // A legacy address after the scheme has no prefix of its own, so it is read bare.
    let legacy = "1BpEi6DfDAUFd7GtittLSdBeYJvcoaVggu";
    assert_eq!(
        ENTRY.validate_address(&format!("bitcoincash:{legacy}?amount=1")).unwrap().value(),
        legacy
    );
}

#[test]
fn payment_uris_refuse_what_jelly_cannot_send() {
    for case in &vectors().unwrap().uris.unsupported {
        assert_eq!(
            ENTRY.validate_address(&case.uri),
            Err(AddressError::UnsupportedRequest { chain: ChainId::BitcoinCash }),
            "{}: {}",
            case.uri,
            case.why
        );
    }
}

/// The scheme is the `CashAddr` prefix, so it stays on the address, and a position the
/// codec reports counts from where the user's text does, request or not.
#[test]
fn a_reported_position_counts_from_the_users_text() {
    let malformed = Err(AddressError::Malformed { chain: ChainId::BitcoinCash, position: 53 });
    assert_eq!(ENTRY.validate_address(OUTSIDE_ALPHABET), malformed);
    assert_eq!(ENTRY.validate_address(&format!("{OUTSIDE_ALPHABET}?amount=1")), malformed);
}

#[test]
fn a_rejected_address_never_appears_in_its_error() {
    assert_rejected_without_leak(
        "validate_address",
        MARKER_PHRASE,
        ENTRY.validate_address(MARKER_PHRASE).map_err(KeyError::from),
    );
}
