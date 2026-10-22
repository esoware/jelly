use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, ScriptKind,
    test_util::{MARKER_PHRASE, assert_rejected_without_leak, key_material_of},
};
use jelly_litecoin::{ENTRY, PARAMS, SPEC};
use jelly_utxo::test_util::{address_at, spendable_kind};

use crate::common::vectors;

#[test]
fn each_derived_vector_path_gives_its_recorded_address() {
    let suite = vectors().unwrap();
    assert_eq!(suite.derived.len(), 5);
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
    assert_eq!(codec.chain(), ChainId::Litecoin);
    for row in &suite.scripts {
        assert_eq!(
            hex::encode(codec.script_pubkey(&row.address).unwrap()),
            row.script_pubkey,
            "{}",
            row.address
        );
        assert_eq!(codec.script_kind(&row.address), row.script_kind, "{}", row.address);
    }
}

/// The superseded `3...` form is read and never written, so it validates to itself
/// rather than to the `M...` spelling of the same script.
#[test]
fn valid_addresses_canonicalise_to_themselves() {
    let suite = vectors().unwrap();
    let scripts = suite.scripts.iter().map(|row| &row.address);
    for case in suite.valid.iter().chain(scripts) {
        let address = ENTRY.validate_address(case).unwrap();
        assert_eq!(address.value(), case);
        assert_eq!(address.chain(), ChainId::Litecoin);
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
        ("m/84'/2'/0'/0/0", ScriptKind::P2wpkh),
        ("m/49'/2'/0'/0/0", ScriptKind::P2shP2wpkh),
        ("m/44'/2'/0'/0/0", ScriptKind::P2pkh),
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

/// A taproot path fails rather than producing a P2WPKH address the user's other
/// wallet would never show.
#[test]
fn litecoin_pays_taproot_without_deriving_it() {
    let refused = |result: Result<String, KeyError>| {
        matches!(
            result,
            Err(KeyError::ScriptUnsupported { chain: ChainId::Litecoin, script: ScriptKind::P2tr })
        )
    };
    assert!(refused(address_at(ENTRY, "m/86'/2'/0'/0/0", None)));
    let key = key_material_of(KeyKind::Secp256k1, vec![0x46; 32]);
    assert!(refused(ENTRY.address(&key, Some(ScriptKind::P2tr))));

    let taproot = "ltc1p5cyxnuxmeuwuvkwfem96lqzszd02n6xdcjrs20cac6yqjjwudpxq4arnzx";
    assert_eq!(ENTRY.validate_address(taproot).unwrap().value(), taproot);
    assert_eq!(ENTRY.codec().script_kind(taproot), Some(ScriptKind::P2tr));
    assert!(!ENTRY.codec().script_pubkey(taproot).unwrap().is_empty());
}

#[test]
fn a_bech32_address_is_read_in_either_case_and_written_in_lower() {
    let lower = "ltc1qjmxnz78nmc8nq77wuxh25n2es7rzm5c2rkk4wh";
    for spelling in [lower.to_owned(), lower.to_uppercase()] {
        assert_eq!(ENTRY.validate_address(&spelling).unwrap().value(), lower);
    }
    let mixed = format!("LTC1{}", lower.get(4..).unwrap());
    assert_eq!(
        ENTRY.validate_address(&mixed),
        Err(AddressError::Malformed { chain: ChainId::Litecoin, position: 4 })
    );
}

/// One Litecoin address of each form this build derives and spends.
const LITECOIN_SPENDABLE: [&str; 3] = [
    "ltc1qjmxnz78nmc8nq77wuxh25n2es7rzm5c2rkk4wh",
    "M7wtsL7wSHDBJVMWWhtQfTMSYYkyooAAXM",
    "LUWPbpM43E2p7ZSh8cyTBEkvpHmr3cB8Ez",
];

#[test]
fn every_litecoin_form_this_build_spends_passes_the_gate_untouched() {
    for address in LITECOIN_SPENDABLE {
        assert_eq!(PARAMS.canonical(address).unwrap(), address);
        spendable_kind(&PARAMS, address).unwrap();
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
            Err(AddressError::UnsupportedRequest { chain: ChainId::Litecoin }),
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
