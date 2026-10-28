use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, KeyMaterial, ScriptKind, SecretText, Transport,
    test_util::{MARKER, MARKER_PHRASE, assert_redacted},
};
use jelly_keypair::{
    Bip32Path, PathTemplate, generate_key, parse_path,
    test_util::{TEST_PHRASE, test_keychain},
};
use jelly_net::DefaultEndpoints;
use jelly_solana::{ENTRY, SPEC};
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::common::USDC_MINT;

const VECTORS_JSON: &str = include_str!("../vectors/addresses.json");

/// The first address of the test seed at the default path.
const ADDRESS: &str = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk";

#[derive(Deserialize)]
struct Vectors {
    derived: Vec<Derived>,
    raw: Vec<Raw>,
    valid: Vec<String>,
    invalid: Vec<Invalid>,
}

#[derive(Deserialize)]
struct Derived {
    path: String,
    address: String,
}

#[derive(Deserialize)]
struct Raw {
    secret: String,
    address: String,
}

#[derive(Deserialize)]
struct Invalid {
    address: String,
}

fn vectors() -> serde_json::Result<Vectors> {
    serde_json::from_str(VECTORS_JSON)
}

fn default_path(index: u32) -> Result<Bip32Path, KeyError> {
    PathTemplate::parse(SPEC.default_path)?.path_at(index)
}

fn derived(path: &Bip32Path) -> Result<KeyMaterial, KeyError> {
    test_keychain().key_material_at(SPEC.key_kind, path)
}

fn address_of(material: &KeyMaterial) -> Result<String, KeyError> {
    ENTRY.address(material, None)
}

fn imported_address(input: &str) -> Result<String, KeyError> {
    address_of(&ENTRY.import_key(input, None)?.material)
}

#[test]
fn each_derived_vector_path_gives_its_recorded_address() {
    for row in &vectors().unwrap().derived {
        assert_eq!(
            address_of(&derived(&parse_path(&row.path).unwrap()).unwrap()).unwrap(),
            row.address,
            "{}",
            row.path
        );
    }
}

#[test]
fn each_raw_vector_key_gives_and_imports_to_its_recorded_address() {
    for row in &vectors().unwrap().raw {
        let bytes = hex::decode(&row.secret).expect("the vector secret is hex");
        let material = KeyMaterial::new(KeyKind::Ed25519Seed, Zeroizing::new(bytes));
        assert_eq!(address_of(&material).unwrap(), row.address);
        assert_eq!(imported_address(&row.secret).unwrap(), row.address);
    }
}

#[test]
fn the_default_path_family_derives_the_documented_addresses() {
    assert_eq!(default_path(0).unwrap(), parse_path("m/44'/501'/0'/0'").unwrap());
    assert_eq!(address_of(&derived(&default_path(0).unwrap()).unwrap()).unwrap(), ADDRESS);
    assert_eq!(
        address_of(&derived(&default_path(1).unwrap()).unwrap()).unwrap(),
        "Hh8QwFUA6MtVu1qAoq12ucvFHNwCcVTV7hpWjeY1Hztb"
    );
}

#[test]
fn valid_addresses_canonicalise_to_themselves() {
    for case in &vectors().unwrap().valid {
        let address = ENTRY.validate_address(case).expect("validate");
        assert_eq!(address.value(), case);
        assert_eq!(address.chain(), ChainId::Solana);
        // Surrounding whitespace is the caller's paste, not part of the value.
        let padded = format!("  {case}  ");
        assert_eq!(ENTRY.validate_address(&padded).expect("validate").value(), case);
    }
}

#[test]
fn invalid_addresses_are_refused() {
    for case in &vectors().unwrap().invalid {
        assert!(ENTRY.validate_address(&case.address).is_err(), "{}", case.address);
    }
    for blank in ["", "   "] {
        assert_eq!(
            ENTRY.validate_address(blank),
            Err(AddressError::Empty { chain: ChainId::Solana }),
            "{blank:?}"
        );
    }
}

#[test]
fn an_invalid_character_is_reported_at_its_own_position() {
    let address = "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpq0";
    assert_eq!(
        ENTRY.validate_address(address),
        Err(AddressError::Malformed { chain: ChainId::Solana, position: 43 })
    );
}

#[test]
fn payment_uris_yield_the_address_they_pay() {
    for uri in [
        format!("solana:{ADDRESS}?amount=1&label=Shop&message=Thanks"),
        format!("SOLANA:{ADDRESS}"),
        format!("solana://{ADDRESS}"),
        format!("  {ADDRESS}\n"),
    ] {
        assert_eq!(ENTRY.validate_address(&uri).expect("validate").value(), ADDRESS, "{uri}");
    }
}

/// `%6Demo` is `memo`, as the Solana Pay reference parser reads it through
/// `URLSearchParams`.
#[test]
fn payment_uris_refuse_a_reference_or_a_memo_in_any_spelling() {
    for query in [
        format!("amount=1&reference={ADDRESS}"),
        String::from("memo=order-7"),
        String::from("amount=1&%6Demo=deposit-7"),
        format!("amount=1&%72eference={ADDRESS}"),
    ] {
        let uri = format!("solana:{ADDRESS}?{query}");
        assert_eq!(
            ENTRY.validate_address(&uri),
            Err(AddressError::UnsupportedRequest { chain: ChainId::Solana }),
            "{uri}"
        );
    }
}

#[test]
fn token_payment_uris_are_refused_instead_of_becoming_native_payments() {
    for name in ["spl-token", "%73pl-token"] {
        let uri = format!("solana:{ADDRESS}?amount=1&{name}={USDC_MINT}");
        assert_eq!(
            ENTRY.validate_address(&uri),
            Err(AddressError::UnsupportedRequest { chain: ChainId::Solana }),
            "{uri}"
        );
        assert_eq!(
            ENTRY.payment_target(&uri),
            Err(AddressError::UnsupportedRequest { chain: ChainId::Solana }),
            "{uri}"
        );
    }
}

#[test]
fn solana_addresses_have_no_script() {
    let codec = ENTRY.codec();
    assert_eq!(codec.chain(), ChainId::Solana);
    assert_eq!(
        codec.script_pubkey(ADDRESS),
        Err(AddressError::NoScript { chain: ChainId::Solana })
    );
    assert_eq!(codec.script_kind(ADDRESS), None);
    assert_eq!(SPEC.resolve_script(Some(44), None).expect("no script"), None);
    assert!(matches!(
        SPEC.resolve_script(None, Some(ScriptKind::P2wpkh)),
        Err(KeyError::ScriptNotApplicable { chain: ChainId::Solana })
    ));
}

#[test]
fn a_solana_address_is_its_public_key() {
    let key = test_keychain().ed25519_at(&default_path(0).unwrap()).expect("a key");
    let address = address_of(&key.key_material()).unwrap();
    let decoded = bs58::decode(&address).into_vec().expect("base58");
    assert_eq!(decoded, key.public_bytes());

    let short = KeyMaterial::new(KeyKind::Ed25519Seed, Zeroizing::new(vec![7; 31]));
    assert!(matches!(ENTRY.address(&short, None), Err(KeyError::InvalidKeyLength { got: 31 })));
    let secp = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(vec![7; 32]));
    let wrong_kind =
        |result| matches!(result, Err(KeyError::WrongKeyKind { chain: ChainId::Solana }));
    assert!(wrong_kind(ENTRY.address(&secp, None).map(|_| ())));
    assert!(wrong_kind(ENTRY.export_key(&secp).map(|_| ())));
    assert!(matches!(
        ENTRY.address(&key.key_material(), Some(ScriptKind::P2wpkh)),
        Err(KeyError::ScriptNotApplicable { chain: ChainId::Solana })
    ));
}

#[test]
fn a_generated_key_has_a_valid_address() {
    let material = generate_key(KeyKind::Ed25519Seed).expect("generate");
    assert_eq!(material.kind(), SPEC.key_kind);
    let address = address_of(&material).unwrap();
    assert_eq!(ENTRY.validate_address(&address).expect("validate").value(), address);
}

#[test]
fn derived_key_material_is_the_spec_kind() {
    let material = derived(&default_path(0).unwrap()).unwrap();
    assert_eq!(material.kind(), KeyKind::Ed25519Seed);
    assert_eq!(material.len(), 32);
    assert!(format!("{material:?}").contains("redacted"));
}

#[test]
fn exported_keys_reimport_to_the_same_address() {
    for index in 0..3 {
        let material = derived(&default_path(index).unwrap()).unwrap();
        let exported = ENTRY.export_key(&material).expect("export");
        assert_eq!(bs58::decode(exported.as_str()).into_vec().expect("base58").len(), 64);
        assert_eq!(imported_address(&exported).unwrap(), address_of(&material).unwrap(), "{index}");
    }
}

#[test]
fn the_vault_bytes_reimport_as_hex() {
    let material = derived(&default_path(0).unwrap()).unwrap();
    let hex_key = material.with_bytes(|bytes| hex::encode(bytes));
    assert_eq!(imported_address(&hex_key).unwrap(), ADDRESS);
    assert_eq!(imported_address(&format!("0x{hex_key}")).unwrap(), ADDRESS);
}

#[test]
fn every_keypair_encoding_imports_the_documented_address() {
    let key = test_keychain().ed25519_at(&default_path(0).unwrap()).expect("a key");
    let keypair = key.to_keypair_bytes();
    let encoded = hex::encode(keypair.as_slice());
    let json = format!("[{}]", keypair.iter().map(u8::to_string).collect::<Vec<_>>().join(","));
    let exported = ENTRY.export_key(&key.key_material()).expect("export");
    for (input, format) in [
        (exported.to_string(), "Solana keypair"),
        (encoded.clone(), "Solana keypair"),
        (format!("0x{}", encoded.to_uppercase()), "Solana keypair"),
        (json, "Solana keypair file"),
        (key.with_secret_bytes(|bytes| hex::encode(bytes)), "32-byte hex key"),
    ] {
        assert_eq!(ENTRY.recognize_key(&input), Some(format), "{input}");
        let imported = ENTRY.import_key(&input, None).expect("import");
        assert_eq!(imported.named_script, None);
        assert_eq!(address_of(&imported.material).unwrap(), ADDRESS, "{input}");
    }
}

#[test]
fn a_password_changes_nothing_about_a_plain_key() {
    let password = SecretText::new(String::from("unused"));
    let exported = ENTRY.export_key(&derived(&default_path(0).unwrap()).unwrap()).expect("export");
    let imported = ENTRY.import_key(&exported, Some(&password)).expect("import");
    assert_eq!(address_of(&imported.material).unwrap(), ADDRESS);
}

/// Another chain's key or a phrase isn't a Solana key, and naming its chain is the chain
/// table's job.
#[test]
fn keys_in_other_encodings_are_not_recognized() {
    for input in [
        "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d",
        TEST_PHRASE,
        r#"{"crypto":{"cipher":"aes-128-ctr"},"version":3}"#,
        "0x1234",
        "",
    ] {
        assert_eq!(ENTRY.recognize_key(input), None, "{input:?}");
        assert!(matches!(ENTRY.import_key(input, None), Err(KeyError::UnknownFormat)), "{input:?}");
    }
}

#[test]
fn solana_has_no_subaddresses() {
    let material = derived(&default_path(0).unwrap()).unwrap();
    assert!(matches!(
        ENTRY.subaddress(&material, 0, 1),
        Err(KeyError::NoSubaddresses { chain: ChainId::Solana })
    ));
    assert!(matches!(
        ENTRY.subaddress_key(&material, 0, 1),
        Err(KeyError::NoSubaddresses { chain: ChainId::Solana })
    ));
}

#[test]
fn the_spec_lists_its_primary_transport_first() {
    assert_eq!(ENTRY.spec(), &SPEC);
    assert_eq!(SPEC.chain, ChainId::Solana);
    assert_eq!(SPEC.transports.first(), Some(&SPEC.primary_transport));
    assert_eq!(SPEC.transport_for("https://rpc.invalid"), Transport::HttpJsonrpc);
    assert_eq!(SPEC.transport_for("wss://rpc.invalid"), Transport::WsJsonrpc);
}

#[test]
fn the_shipped_endpoints_are_solana_endpoints_on_its_transports() {
    let defaults: DefaultEndpoints =
        serde_json::from_str(ENTRY.default_endpoints()).expect("the endpoints file parses");
    assert_eq!(defaults.chain, ChainId::Solana);
    for transport in SPEC.transports {
        assert!(
            defaults.endpoints.iter().any(|endpoint| endpoint.transport == *transport),
            "{transport}"
        );
    }
    for endpoint in &defaults.endpoints {
        assert!(SPEC.transports.contains(&endpoint.transport), "{}", endpoint.url);
    }
}

#[test]
fn a_rejected_address_or_key_never_appears_in_its_error() {
    let refused = ENTRY.validate_address(MARKER_PHRASE).expect_err("not an address");
    assert_redacted("validate_address", MARKER_PHRASE, &KeyError::from(refused));
    for input in [MARKER, MARKER_PHRASE] {
        let refused = ENTRY.import_key(input, None).expect_err("not a key");
        assert_redacted("import_key", input, &refused);
    }
}

#[test]
fn an_imported_key_redacts_its_debug() {
    let material = derived(&default_path(0).unwrap()).unwrap();
    let hex_key = material.with_bytes(|bytes| hex::encode(bytes));
    let imported = ENTRY.import_key(&hex_key, None).expect("import");
    let debug = format!("{imported:?}");
    assert!(!debug.contains(&hex_key));
    let exported = ENTRY.export_key(&material).expect("export");
    assert!(!debug.contains(exported.as_str()));
}
