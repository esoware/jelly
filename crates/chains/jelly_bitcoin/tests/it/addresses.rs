use jelly_bitcoin::{ENTRY, SPEC};
use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, ScriptKind,
    test_util::{MARKER_PHRASE, assert_rejected_without_leak, key_material_of},
};
use jelly_keypair::{PathTemplate, parse_path};
use jelly_utxo::test_util::address_at;

use crate::common::vectors;

#[test]
fn each_derived_vector_path_gives_its_recorded_address() {
    let suite = vectors().unwrap();
    assert_eq!(suite.derived.len(), 8);
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
    assert_eq!(suite.scripts.len(), 6);
    let codec = ENTRY.codec();
    assert_eq!(codec.chain(), ChainId::Bitcoin);
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
        assert_eq!(address.chain(), ChainId::Bitcoin);
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
    for (text, kind) in [
        ("m/86'/0'/0'/0/0", ScriptKind::P2tr),
        ("m/84'/0'/0'/0/0", ScriptKind::P2wpkh),
        ("m/49'/0'/0'/0/0", ScriptKind::P2shP2wpkh),
        ("m/44'/0'/0'/0/0", ScriptKind::P2pkh),
    ] {
        let address = address_at(ENTRY, text, None).unwrap();
        assert_eq!(ENTRY.codec().script_kind(&address), Some(kind), "{text}");
        assert!(!ENTRY.codec().script_pubkey(&address).unwrap().is_empty());
    }
}

#[test]
fn a_path_without_a_purpose_takes_the_chains_default_flavour() {
    assert_eq!(SPEC.default_script, Some(ScriptKind::P2wpkh));
    assert_eq!(
        address_at(ENTRY, "m/0/0", None).unwrap(),
        address_at(ENTRY, "m/0/0", Some(ScriptKind::P2wpkh)).unwrap()
    );
}

/// BIP-84 publishes `bc1qcr8te...` as the first receive address of the all-abandon
/// phrase, which is what a new Bitcoin address of that wallet has to be.
#[test]
fn the_first_default_address_is_the_published_bip84_one() {
    let first = PathTemplate::parse(SPEC.default_path).unwrap().path_at(0).unwrap();
    assert_eq!(first, parse_path("m/84'/0'/0'/0/0").unwrap());
    assert_eq!(
        address_at(ENTRY, "m/84'/0'/0'/0/0", None).unwrap(),
        "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu"
    );
}

#[test]
fn bitcoin_tells_a_bad_checksum_from_another_networks_address() {
    let checksum = AddressError::Checksum { chain: ChainId::Bitcoin };
    let network = AddressError::WrongNetwork { chain: ChainId::Bitcoin };
    for (address, want) in [
        ("bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyv", checksum),
        ("1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabB", checksum),
        ("ltc1qjmxnz78nmc8nq77wuxh25n2es7rzm5c2rkk4wh", network),
        ("LUWPbpM43E2p7ZSh8cyTBEkvpHmr3cB8Ez", network),
    ] {
        assert_eq!(ENTRY.validate_address(address), Err(want), "{address}");
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
            Err(AddressError::UnsupportedRequest { chain: ChainId::Bitcoin }),
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
