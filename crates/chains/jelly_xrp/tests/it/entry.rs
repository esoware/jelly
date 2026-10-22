use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind,
    test_util::{MARKER_PHRASE, assert_redacted},
};
use jelly_keypair::{HdKeychain, Seed, generate_key, parse_path};
use jelly_xrp::{ENTRY, SPEC};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
struct Vectors {
    seed: String,
    derived: Vec<Derived>,
}

#[derive(Deserialize)]
struct Derived {
    path: String,
    secret: String,
    address: String,
}

fn vectors() -> Result<Vectors, serde_json::Error> {
    serde_json::from_str(include_str!("../vectors/addresses.json"))
}

#[test]
fn derived_keys_match_the_published_addresses_and_secrets_at_the_default_path() {
    let suite = vectors().unwrap();
    let keychain =
        HdKeychain::from_seed(Seed::from_bytes(&hex::decode(&suite.seed).unwrap()).unwrap());
    for row in &suite.derived {
        let material =
            keychain.key_material_at(SPEC.key_kind, &parse_path(&row.path).unwrap()).unwrap();
        assert_eq!(material.kind(), KeyKind::Secp256k1);
        material.with_bytes(|bytes| assert_eq!(bytes.len(), 32));
        assert!(format!("{material:?}").contains("redacted"));
        assert_eq!(ENTRY.address(&material, None).unwrap(), row.address, "{}", row.path);
        assert_eq!(ENTRY.export_key(&material).unwrap().as_str(), row.secret, "{}", row.path);
    }
    let first = suite.derived.first().unwrap();
    assert_eq!(SPEC.default_path.replace("{index}", "0"), first.path);
}

#[test]
fn an_exported_key_imports_to_the_same_address_with_or_without_a_prefix() {
    for row in vectors().unwrap().derived {
        for input in [row.secret.clone(), format!(" 0x{} ", row.secret.to_uppercase())] {
            assert_eq!(ENTRY.recognize_key(&input), Some("32-byte hex key"));
            let imported = ENTRY.import_key(&input, None).unwrap();
            assert_eq!(imported.named_script, None);
            assert_eq!(imported.material.kind(), KeyKind::Secp256k1);
            assert_eq!(ENTRY.address(&imported.material, None).unwrap(), row.address);
            assert_eq!(ENTRY.export_key(&imported.material).unwrap().as_str(), row.secret);
        }
    }
}

#[test]
fn text_in_no_accepted_encoding_is_an_unknown_format_and_is_not_claimed() {
    for input in [
        "",
        "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d",
        &"ab".repeat(31),
        &"ab".repeat(64),
        MARKER_PHRASE,
    ] {
        assert!(matches!(ENTRY.import_key(input, None), Err(KeyError::UnknownFormat)), "{input}");
        assert_eq!(ENTRY.recognize_key(input), None, "{input}");
    }
}

#[test]
fn a_hex_key_outside_the_curve_order_is_refused_as_invalid() {
    let outside = "ff".repeat(32);
    assert_eq!(ENTRY.recognize_key(&outside), Some("32-byte hex key"));
    assert!(matches!(ENTRY.import_key(&outside, None), Err(KeyError::InvalidSecretKey)));
}

#[test]
fn a_generated_key_produces_an_address_the_codec_accepts() {
    let material = generate_key(SPEC.key_kind).unwrap();
    let address = ENTRY.address(&material, None).unwrap();
    assert_eq!(ENTRY.validate_address(&address).unwrap().value(), address);
    assert_eq!(ENTRY.codec().validate(&address).unwrap().chain(), ChainId::Xrp);
}

#[test]
fn a_key_of_another_curve_has_no_address_or_export() {
    let material = generate_key(KeyKind::Ed25519Seed).unwrap();
    assert!(matches!(ENTRY.address(&material, None), Err(KeyError::InvalidSecretKey)));
    assert!(matches!(ENTRY.export_key(&material), Err(KeyError::InvalidSecretKey)));
}

#[test]
fn empty_and_blank_addresses_are_refused() {
    for value in ["", "   "] {
        assert_eq!(ENTRY.validate_address(value), Err(AddressError::Empty { chain: ChainId::Xrp }));
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
    assert_eq!(document["chain"], ChainId::Xrp.as_str());
    let endpoints = document["endpoints"].as_array().unwrap();
    assert!(!endpoints.is_empty());
    for endpoint in endpoints {
        let transport = endpoint["transport"].as_str().unwrap();
        assert!(SPEC.transports.iter().any(|known| known.as_str() == transport), "{transport}");
    }
    assert_eq!(SPEC.transports.first(), Some(&SPEC.primary_transport));
}
