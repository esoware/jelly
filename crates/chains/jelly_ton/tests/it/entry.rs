use std::collections::BTreeMap;

use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, ScriptKind, SecretText,
    test_util::{MARKER_PHRASE, assert_redacted},
};
use jelly_keypair::{HdKeychain, Seed, generate_key, parse_path, test_util::TEST_PHRASE};
use jelly_ton::{ENTRY, SPEC};
use serde::Deserialize;
use serde_json::Value;
use tonlib_core::types::TonAddress;

const TON_SEED: &str = "dose ice enrich trigger test dove century still betray gas diet dune use other base gym mad law immense village world example praise game";
const PROTECTED_TON_SEED: &str = "atom trumpet engine wisdom butter embark outer brand quiz gaze moment second daughter motion vault banner auction weasel fire kid use away cry cover";

const DEFAULT: Option<ScriptKind> = Some(ScriptKind::TonV5r1);

#[derive(Deserialize)]
struct Suite {
    seed: String,
    derived: Vec<Derived>,
}

#[derive(Deserialize)]
struct Derived {
    path: String,
    secret: String,
    addresses: BTreeMap<ScriptKind, String>,
}

fn suites() -> Result<Vec<Suite>, serde_json::Error> {
    serde_json::from_str(include_str!("../vectors/addresses.json"))
}

#[test]
fn derived_keys_match_the_published_addresses_and_secrets_at_the_default_path() {
    for suite in suites().unwrap() {
        let keychain =
            HdKeychain::from_seed(Seed::from_bytes(&hex::decode(&suite.seed).unwrap()).unwrap());
        for row in &suite.derived {
            let material =
                keychain.key_material_at(SPEC.key_kind, &parse_path(&row.path).unwrap()).unwrap();
            assert_eq!(material.kind(), KeyKind::Ed25519Seed);
            material.with_bytes(|bytes| assert_eq!(bytes.len(), 32));
            assert!(format!("{material:?}").contains("redacted"));
            for (script, address) in &row.addresses {
                assert_eq!(ENTRY.address(&material, Some(*script)).unwrap(), *address, "{script}");
            }
            assert_eq!(ENTRY.export_key(&material).unwrap().as_str(), row.secret, "{}", row.path);
            assert_eq!(SPEC.default_path.replace("{index}", "0"), row.path);
        }
    }
}

#[test]
fn an_exported_key_imports_to_the_same_address_with_or_without_a_prefix() {
    for row in suites().unwrap().into_iter().flat_map(|suite| suite.derived) {
        for input in [row.secret.clone(), format!(" 0x{} ", row.secret.to_uppercase())] {
            assert_eq!(ENTRY.recognize_key(&input), Some("32-byte hex key"));
            let imported = ENTRY.import_key(&input, None).unwrap();
            assert_eq!(imported.named_script, None);
            assert_eq!(imported.material.kind(), KeyKind::Ed25519Seed);
            for (script, address) in &row.addresses {
                assert_eq!(ENTRY.address(&imported.material, Some(*script)).unwrap(), *address);
            }
            assert_eq!(ENTRY.export_key(&imported.material).unwrap().as_str(), row.secret);
        }
    }
}

/// tonlib-core 0.26.11's `test_ton_wallet_create`, which derives the V3R2 and V4R2
/// wallets of a TON mnemonic as the TON wallets that use one show them.
#[test]
fn a_ton_seed_holds_its_v3r2_and_v4r2_wallets_at_the_reference_addresses() {
    let phrase = "fancy carpet hello mandate penalty trial consider property top vicious exit rebuild tragic profit urban major total month holiday sudden rib gather media vicious";
    let material = ENTRY.import_key(phrase, None).unwrap().material;
    for (script, expected) in [
        (ScriptKind::TonV3r2, "EQA-RswW9QONn88ziVm4UKnwXDEot5km7GEEXsfie_0TFOCO"),
        (ScriptKind::TonV4r2, "EQCDM_QGggZ3qMa_f3lRPk4_qLDnLTqdi6OkMAV2NB9r5TG3"),
    ] {
        let derived = ENTRY.address(&material, Some(script)).unwrap();
        assert_eq!(
            TonAddress::from_base64_url(&derived).unwrap(),
            TonAddress::from_base64_url(expected).unwrap(),
            "{script}"
        );
    }
}

#[test]
fn the_wallet_versions_are_flavours_no_path_names_and_v5r1_is_the_default() {
    assert_eq!(SPEC.resolve_script(Some(44), None).unwrap(), DEFAULT);
    assert_eq!(
        SPEC.resolve_script(Some(44), Some(ScriptKind::TonV4r2)).unwrap(),
        Some(ScriptKind::TonV4r2)
    );
    assert!(matches!(
        SPEC.resolve_script(None, Some(ScriptKind::P2wpkh)),
        Err(KeyError::ScriptUnsupported { script: ScriptKind::P2wpkh, .. })
    ));
    let material = generate_key(SPEC.key_kind).unwrap();
    let addresses: Vec<String> = SPEC
        .script_kinds
        .iter()
        .map(|script| ENTRY.address(&material, Some(*script)).unwrap())
        .collect();
    assert_eq!(addresses.len(), 3);
    assert!(
        addresses
            .iter()
            .all(|address| addresses.iter().filter(|other| *other == address).count() == 1)
    );
    ENTRY.address(&material, None).unwrap_err();
}

#[test]
fn a_ton_seed_is_named_and_imports_with_its_password() {
    assert_eq!(ENTRY.recognize_key(TON_SEED), Some("TON seed"));
    assert_eq!(ENTRY.recognize_key(PROTECTED_TON_SEED), Some("TON seed"));
    let password = SecretText::new(String::from("public-test-password"));
    let imported = ENTRY.import_key(PROTECTED_TON_SEED, Some(&password)).unwrap();
    assert_eq!(imported.material.kind(), KeyKind::Ed25519Seed);
    assert_eq!(
        ENTRY.export_key(&imported.material).unwrap().as_str(),
        "dc03db4f1944a7522e2310b5f4ff366a5511e2dbd355274f829cc5c303d4cd35"
    );
    assert!(matches!(ENTRY.import_key(PROTECTED_TON_SEED, None), Err(KeyError::PasswordRequired)));
}

/// Naming a BIP-39 seed phrase is the engine's job, so the entry reports why the
/// phrase is no TON seed.
#[test]
fn a_seed_phrase_fails_as_a_ton_seed() {
    assert_eq!(ENTRY.recognize_key(TEST_PHRASE), None);
    assert!(matches!(
        ENTRY.import_key(TEST_PHRASE, None),
        Err(KeyError::TonSeedWordCount { got: 12 })
    ));
    let twenty_four = format!("{} art", ["abandon"; 23].join(" "));
    assert!(matches!(ENTRY.import_key(&twenty_four, None), Err(KeyError::PhraseChecksum)));
}

#[test]
fn text_in_no_accepted_encoding_is_an_unknown_format_and_is_not_claimed() {
    for input in [
        "",
        "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d",
        &"ab".repeat(31),
        &"ab".repeat(64),
    ] {
        assert!(matches!(ENTRY.import_key(input, None), Err(KeyError::UnknownFormat)), "{input}");
        assert_eq!(ENTRY.recognize_key(input), None, "{input}");
    }
}

#[test]
fn a_generated_key_produces_an_address_the_codec_accepts() {
    let material = generate_key(SPEC.key_kind).unwrap();
    let address = ENTRY.address(&material, DEFAULT).unwrap();
    assert_eq!(ENTRY.validate_address(&address).unwrap().value(), address);
    assert_eq!(ENTRY.codec().validate(&address).unwrap().chain(), ChainId::Ton);
    let request = format!("ton://transfer/{address}?amount=1&text=order%207");
    assert_eq!(ENTRY.validate_address(&request).unwrap().value(), address);
}

#[test]
fn a_key_of_another_curve_has_no_address_or_export() {
    let material = generate_key(KeyKind::Secp256k1).unwrap();
    assert!(matches!(ENTRY.address(&material, DEFAULT), Err(KeyError::InvalidSecretKey)));
    assert!(matches!(ENTRY.export_key(&material), Err(KeyError::InvalidSecretKey)));
}

#[test]
fn empty_and_blank_addresses_are_refused() {
    for value in ["", "   "] {
        assert_eq!(ENTRY.validate_address(value), Err(AddressError::Empty { chain: ChainId::Ton }));
    }
}

#[test]
fn a_rejected_address_or_key_never_appears_in_its_error() {
    let refused = ENTRY.validate_address(MARKER_PHRASE).unwrap_err();
    assert_redacted("validate_address", MARKER_PHRASE, &refused);
    let refused = ENTRY.import_key(MARKER_PHRASE, None).unwrap_err();
    assert_redacted("import_key", MARKER_PHRASE, &refused);
}

#[test]
fn the_shipped_endpoints_belong_to_this_chain_and_its_transports() {
    let document: Value = serde_json::from_str(ENTRY.default_endpoints()).unwrap();
    assert_eq!(document["chain"], ChainId::Ton.as_str());
    let endpoints = document["endpoints"].as_array().unwrap();
    assert!(!endpoints.is_empty());
    for endpoint in endpoints {
        let transport = endpoint["transport"].as_str().unwrap();
        assert!(SPEC.transports.iter().any(|known| known.as_str() == transport), "{transport}");
    }
    assert_eq!(SPEC.transports.first(), Some(&SPEC.primary_transport));
}
