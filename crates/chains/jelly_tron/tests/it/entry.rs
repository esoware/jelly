use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, KeyMaterial, ScriptKind, SecretText, Transport,
    test_util::{MARKER, MARKER_PHRASE, assert_redacted},
};
use jelly_keypair::{
    Bip32Path, PathTemplate, generate_key, parse_path,
    test_util::{TEST_PHRASE, test_keychain},
};
use jelly_net::DefaultEndpoints;
use jelly_tron::{ENTRY, SPEC};
use serde::Deserialize;
use zeroize::Zeroizing;

const VECTORS_JSON: &str = include_str!("../vectors/addresses.json");

/// The first address of the test seed at the default path.
const ADDRESS: &str = "TUEZSdKsoDHQMeZwihtdoBiN46zxhGWYdH";

/// The `MetaMask` default key of the test seed and its published Ethereum address.
const ETHEREUM_SECRET: &str = "1ab42cc412b618bdea3a599e3c9bae199ebf030895b039e9db1e30dafb12b727";
const ETHEREUM_ADDRESS: &str = "9858effd232b4033e47d90003d41ec34ecaeda94";

#[derive(Deserialize)]
struct Vectors {
    derived: Vec<Derived>,
    raw: Vec<Raw>,
    valid: Vec<String>,
    invalid: Vec<Invalid>,
    keystore: Keystore,
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

#[derive(Deserialize)]
struct Keystore {
    password: String,
    secret: String,
    evm_address: String,
    pbkdf2: String,
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

/// The TRON spelling of an EVM address, encoded by `bs58`, not by the code under test.
fn tron_of_evm(evm_hex: &str) -> Result<String, hex::FromHexError> {
    let mut body = vec![0x41];
    body.extend(hex::decode(evm_hex)?);
    Ok(bs58::encode(body).with_check().into_string())
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
        let material = KeyMaterial::new(KeyKind::Secp256k1, Zeroizing::new(bytes));
        assert_eq!(address_of(&material).unwrap(), row.address);
        assert_eq!(imported_address(&row.secret).unwrap(), row.address);
    }
}

#[test]
fn the_default_path_family_derives_the_documented_addresses() {
    assert_eq!(default_path(0).unwrap(), parse_path("m/44'/195'/0'/0/0").unwrap());
    assert_eq!(address_of(&derived(&default_path(0).unwrap()).unwrap()).unwrap(), ADDRESS);
    assert_eq!(
        address_of(&derived(&default_path(1).unwrap()).unwrap()).unwrap(),
        "TSeJkUh4Qv67VNFwY8LaAxERygNdy6NQZK"
    );
}

#[test]
fn valid_addresses_canonicalise_to_themselves() {
    for case in &vectors().unwrap().valid {
        let address = ENTRY.validate_address(case).expect("validate");
        assert_eq!(address.value(), case);
        assert_eq!(address.chain(), ChainId::Tron);
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
            Err(AddressError::Empty { chain: ChainId::Tron }),
            "{blank:?}"
        );
    }
}

#[test]
fn payment_uris_yield_the_address_they_pay() {
    for uri in [
        format!("tron:{ADDRESS}?amount=5"),
        format!("TRON:{ADDRESS}"),
        format!("tron://{ADDRESS}"),
        format!("  {ADDRESS}\n"),
    ] {
        assert_eq!(ENTRY.validate_address(&uri).expect("validate").value(), ADDRESS, "{uri}");
    }
}

#[test]
fn payment_uris_refuse_parameters_that_could_change_a_native_payment() {
    for query in [
        "memo=order-7",
        "%6Demo=order-7",
        "token=USDT",
        "tokenId=1002000",
        "contract=TR7NHqjeKQxGTCi8q8ZY4pL8otSzgjLj6t",
        "amount=5&%74oken=USDT",
    ] {
        let uri = format!("tron:{ADDRESS}?{query}");
        assert_eq!(
            ENTRY.validate_address(&uri),
            Err(AddressError::UnsupportedRequest { chain: ChainId::Tron }),
            "{uri}"
        );
        assert_eq!(
            ENTRY.payment_target(&uri),
            Err(AddressError::UnsupportedRequest { chain: ChainId::Tron }),
            "{uri}"
        );
    }
}

#[test]
fn tron_addresses_have_no_script() {
    let codec = ENTRY.codec();
    assert_eq!(codec.chain(), ChainId::Tron);
    assert_eq!(codec.script_pubkey(ADDRESS), Err(AddressError::NoScript { chain: ChainId::Tron }));
    assert_eq!(codec.script_kind(ADDRESS), None);
    assert_eq!(SPEC.resolve_script(Some(44), None).expect("no script"), None);
    assert!(matches!(
        SPEC.resolve_script(None, Some(ScriptKind::P2wpkh)),
        Err(KeyError::ScriptNotApplicable { chain: ChainId::Tron })
    ));
    assert!(matches!(
        ENTRY.address(&derived(&default_path(0).unwrap()).unwrap(), Some(ScriptKind::P2pkh)),
        Err(KeyError::ScriptNotApplicable { chain: ChainId::Tron })
    ));
}

#[test]
fn a_tron_address_is_the_ethereum_address_behind_its_prefix() {
    assert_eq!(imported_address(ETHEREUM_SECRET).unwrap(), tron_of_evm(ETHEREUM_ADDRESS).unwrap());
    assert_eq!(
        imported_address(&format!("0x{ETHEREUM_SECRET}")).unwrap(),
        tron_of_evm(ETHEREUM_ADDRESS).unwrap()
    );
    let ethereum = derived(&parse_path("m/44'/60'/0'/0/0").unwrap()).unwrap();
    assert_eq!(ethereum.with_bytes(|bytes| hex::encode(bytes)), ETHEREUM_SECRET);
    assert_eq!(address_of(&ethereum).unwrap(), tron_of_evm(ETHEREUM_ADDRESS).unwrap());
}

#[test]
fn a_key_of_another_kind_has_no_tron_address() {
    let ed25519 = KeyMaterial::new(KeyKind::Ed25519Seed, Zeroizing::new(vec![7; 32]));
    let wrong_kind =
        |result| matches!(result, Err(KeyError::WrongKeyKind { chain: ChainId::Tron }));
    assert!(wrong_kind(ENTRY.address(&ed25519, None).map(|_| ())));
    assert!(wrong_kind(ENTRY.export_key(&ed25519).map(|_| ())));
}

#[test]
fn a_generated_key_has_a_valid_address() {
    let material = generate_key(KeyKind::Secp256k1).expect("generate");
    assert_eq!(material.kind(), SPEC.key_kind);
    let address = address_of(&material).unwrap();
    assert_eq!(ENTRY.validate_address(&address).expect("validate").value(), address);
}

#[test]
fn derived_key_material_is_the_spec_kind() {
    let material = derived(&default_path(0).unwrap()).unwrap();
    assert_eq!(material.kind(), KeyKind::Secp256k1);
    assert_eq!(material.len(), 32);
    assert!(format!("{material:?}").contains("redacted"));
}

#[test]
fn exported_keys_are_bare_hex_and_reimport_to_the_same_address() {
    for index in 0..3 {
        let material = derived(&default_path(index).unwrap()).unwrap();
        let exported = ENTRY.export_key(&material).expect("export");
        assert_eq!(exported.as_str(), material.with_bytes(|bytes| hex::encode(bytes)));
        assert_eq!(imported_address(&exported).unwrap(), address_of(&material).unwrap(), "{index}");
        assert_eq!(ENTRY.recognize_key(&exported), Some("32-byte hex key"));
    }
}

#[test]
fn a_hex_scalar_outside_the_curve_order_is_refused() {
    let input = "ff".repeat(32);
    assert_eq!(ENTRY.recognize_key(&input), Some("32-byte hex key"));
    assert!(matches!(ENTRY.import_key(&input, None), Err(KeyError::InvalidSecretKey)));
}

#[test]
fn a_keystore_document_asks_for_its_password_and_then_imports() {
    let suite = vectors().unwrap().keystore;
    assert_eq!(ENTRY.recognize_key(&suite.pbkdf2), Some("keystore file"));
    assert!(matches!(ENTRY.import_key(&suite.pbkdf2, None), Err(KeyError::PasswordRequired)));
    assert!(matches!(
        ENTRY.import_key(&suite.pbkdf2, Some(&SecretText::new(String::from("not it")))),
        Err(KeyError::WrongPassword)
    ));

    let password = SecretText::new(suite.password.clone());
    let imported = ENTRY.import_key(&suite.pbkdf2, Some(&password)).expect("import");
    assert_eq!(imported.named_script, None);
    assert_eq!(address_of(&imported.material).unwrap(), tron_of_evm(&suite.evm_address).unwrap());
    assert_eq!(ENTRY.export_key(&imported.material).expect("export").as_str(), suite.secret);
}

/// Another chain's key or a phrase isn't a TRON key. The chain table says what it is
/// instead.
#[test]
fn keys_in_other_encodings_are_not_recognized() {
    for input in [
        "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d",
        TEST_PHRASE,
        "[7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7]",
        &"ab".repeat(64),
        "0x1234",
        "",
    ] {
        assert_eq!(ENTRY.recognize_key(input), None, "{input:?}");
        assert!(matches!(ENTRY.import_key(input, None), Err(KeyError::UnknownFormat)), "{input:?}");
    }
}

#[test]
fn tron_has_no_subaddresses() {
    let material = derived(&default_path(0).unwrap()).unwrap();
    assert!(matches!(
        ENTRY.subaddress(&material, 0, 1),
        Err(KeyError::NoSubaddresses { chain: ChainId::Tron })
    ));
    assert!(matches!(
        ENTRY.subaddress_key(&material, 0, 1),
        Err(KeyError::NoSubaddresses { chain: ChainId::Tron })
    ));
}

#[test]
fn the_spec_lists_its_primary_transport_first() {
    assert_eq!(ENTRY.spec(), &SPEC);
    assert_eq!(SPEC.chain, ChainId::Tron);
    assert_eq!(SPEC.transports, [Transport::TronHttp]);
    assert_eq!(SPEC.transports.first(), Some(&SPEC.primary_transport));
    assert_eq!(SPEC.transport_for("https://node.invalid"), Transport::TronHttp);
}

#[test]
fn the_shipped_endpoints_are_tron_endpoints_on_its_transport() {
    let defaults: DefaultEndpoints =
        serde_json::from_str(ENTRY.default_endpoints()).expect("the endpoints file parses");
    assert_eq!(defaults.chain, ChainId::Tron);
    assert!(!defaults.endpoints.is_empty());
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
    let hex_key = format!("0x{}", "ff".repeat(32));
    let refused = ENTRY.import_key(&hex_key, None).expect_err("outside the curve order");
    assert_redacted("import_key", &hex_key, &refused);
}

#[test]
fn a_rejected_keystore_never_leaks_its_password() {
    let suite = vectors().unwrap().keystore;
    let password = SecretText::new(String::from("zzzzwrongpassword"));
    let refused = ENTRY.import_key(&suite.pbkdf2, Some(&password)).expect_err("wrong password");
    assert_redacted("import_key", "zzzzwrongpassword", &refused);
    assert_redacted("import_key", &suite.pbkdf2, &refused);
}

#[test]
fn an_imported_key_redacts_its_debug() {
    let imported = ENTRY.import_key(ETHEREUM_SECRET, None).expect("import");
    assert!(!format!("{imported:?}").contains(ETHEREUM_SECRET));
}
