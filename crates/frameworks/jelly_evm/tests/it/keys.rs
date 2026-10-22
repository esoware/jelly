use jelly_core::{
    AddressError, ChainEntry, KeyError, KeyKind, ScriptKind, SecretText,
    test_util::{MARKER_PHRASE, assert_redacted, key_material},
};
use jelly_evm::test_util::PAYEE;
use jelly_keypair::test_util::TEST_PHRASE;
use serde::Deserialize;

use crate::common::{ENTRIES, Fallible};

const VECTORS_JSON: &str = include_str!("../vectors/addresses.json");
const KEYSTORE_JSON: &str =
    include_str!("../../../../jelly_keypair/tests/vectors/keystore_v3.json");

const USDT_CONTRACT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";

const ETHEREUM: &dyn ChainEntry = jelly_ethereum::ENTRY;
const BSC: &dyn ChainEntry = jelly_bsc::ENTRY;
const POLYGON: &dyn ChainEntry = jelly_polygon::ENTRY;

const KEYSTORE_FORMAT: &str = "keystore file";
const HEX_KEY_FORMAT: &str = "32-byte hex key";

#[derive(Deserialize)]
struct Vectors {
    raw: Vec<Raw>,
    eip55: Vec<String>,
    invalid: Vec<Invalid>,
}

#[derive(Deserialize)]
struct Raw {
    secret: String,
    address: String,
}

#[derive(Deserialize)]
struct Invalid {
    address: String,
    why: String,
}

#[derive(Deserialize)]
struct Keystore {
    password: String,
    secret: String,
    address: String,
    pbkdf2: String,
    scrypt: String,
}

fn vectors() -> Fallible<Vectors> {
    Ok(serde_json::from_str(VECTORS_JSON)?)
}

fn keystore() -> Fallible<Keystore> {
    Ok(serde_json::from_str(KEYSTORE_JSON)?)
}

#[test]
fn a_raw_key_holds_its_published_address_and_exports_back_on_every_network() {
    for row in &vectors().unwrap().raw {
        for entry in ENTRIES {
            let imported = entry.import_key(&row.secret, None).unwrap();
            assert_eq!(imported.named_script, None);
            assert_eq!(entry.address(&imported.material, None).unwrap(), row.address);
            assert_eq!(entry.export_key(&imported.material).unwrap().as_str(), row.secret);
        }
    }
}

#[test]
fn a_hex_key_reads_with_either_prefix_and_surrounding_space() {
    for row in &vectors().unwrap().raw {
        for input in [
            format!("0x{}", row.secret),
            format!("0X{}", row.secret),
            format!("  {}\n", row.secret),
        ] {
            let imported = ETHEREUM.import_key(&input, None).unwrap();
            assert_eq!(ETHEREUM.address(&imported.material, None).unwrap(), row.address, "{input}");
            assert_eq!(ETHEREUM.recognize_key(&input), Some(HEX_KEY_FORMAT), "{input}");
        }
    }
}

#[test]
fn text_in_no_accepted_encoding_is_an_unknown_format() {
    for entry in ENTRIES {
        for input in [
            "",
            "   ",
            "0x1234",
            "[1,2,3]",
            &"zz".repeat(32),
            &"ab".repeat(33),
            TEST_PHRASE,
            "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d",
        ] {
            assert!(
                matches!(entry.import_key(input, None), Err(KeyError::UnknownFormat)),
                "{input:?}"
            );
            assert_eq!(entry.recognize_key(input), None, "{input:?}");
        }
    }
}

#[test]
fn a_hex_key_outside_the_curve_order_is_refused() {
    let zero = "00".repeat(32);
    assert!(matches!(ETHEREUM.import_key(&zero, None), Err(KeyError::InvalidSecretKey)));
    assert_eq!(ETHEREUM.recognize_key(&zero), Some(HEX_KEY_FORMAT), "the shape alone is named");
}

#[test]
fn a_script_type_or_another_curve_has_no_evm_address() {
    let secret = &vectors().unwrap().raw[0].secret;
    let imported = BSC.import_key(secret, None).unwrap();
    let bsc = BSC.spec().chain;
    assert!(matches!(
        BSC.address(&imported.material, Some(ScriptKind::P2wpkh)),
        Err(KeyError::ScriptNotApplicable { chain }) if chain == bsc
    ));
    let ed25519 = key_material(KeyKind::Ed25519Seed, 0x46);
    let ethereum = ETHEREUM.spec().chain;
    assert!(matches!(
        ETHEREUM.address(&ed25519, None),
        Err(KeyError::WrongKeyKind { chain }) if chain == ethereum
    ));
    assert!(matches!(
        ETHEREUM.export_key(&ed25519),
        Err(KeyError::WrongKeyKind { chain }) if chain == ethereum
    ));
}

#[test]
fn the_eip_55_examples_read_back_checksummed_on_every_network() {
    for case in &vectors().unwrap().eip55 {
        let body = case.strip_prefix("0x").unwrap();
        for entry in ENTRIES {
            assert_eq!(entry.validate_address(case).unwrap().value(), case);
            for single_case in
                [case.to_lowercase(), format!("0x{}", body.to_uppercase()), body.to_lowercase()]
            {
                assert_eq!(
                    entry.validate_address(&single_case).unwrap().value(),
                    case,
                    "single-case input is re-checksummed"
                );
            }
        }
    }
    // A mixed-case address with a wrong checksum is refused rather than fixed.
    let mangled = "0x52908400098527886E0F7030069857D2E4169Ee7";
    assert_eq!(
        ETHEREUM.validate_address(mangled),
        Err(AddressError::Checksum { chain: ETHEREUM.spec().chain })
    );
}

#[test]
fn the_invalid_address_vectors_are_refused_on_every_network() {
    let suite = vectors().unwrap();
    for entry in ENTRIES {
        for case in &suite.invalid {
            assert!(
                entry.validate_address(&case.address).is_err(),
                "{}: {}",
                case.why,
                case.address
            );
        }
        for blank in ["", "   "] {
            assert_eq!(
                entry.validate_address(blank),
                Err(AddressError::Empty { chain: entry.spec().chain })
            );
        }
    }
}

#[test]
fn valid_addresses_canonicalise_to_themselves_whatever_surrounds_them() {
    for entry in ENTRIES {
        for padded in [format!("  {PAYEE}  "), format!("\n{PAYEE}\t")] {
            let address = entry.validate_address(&padded).unwrap();
            assert_eq!(address.value(), PAYEE);
            assert_eq!(address.chain(), entry.spec().chain);
        }
    }
}

#[test]
fn a_codec_reads_no_script() {
    for entry in ENTRIES {
        let codec = entry.codec();
        assert_eq!(codec.chain(), entry.spec().chain);
        assert_eq!(
            codec.script_pubkey(PAYEE),
            Err(AddressError::NoScript { chain: entry.spec().chain })
        );
        assert_eq!(codec.script_kind(PAYEE), None);
        assert_eq!(codec.address_of(&[0x6a, 0x01, 0x00]), None);
    }
}

#[test]
fn payment_uris_yield_the_address_they_pay() {
    let cases = [
        (ETHEREUM, format!("ethereum:{}", PAYEE.to_lowercase())),
        (ETHEREUM, format!("ETHEREUM:{PAYEE}")),
        (ETHEREUM, format!("ethereum://{PAYEE}")),
        (ETHEREUM, format!("ethereum:pay-{PAYEE}@1?value=1e18")),
        (BSC, format!("ethereum:{PAYEE}")),
        (BSC, format!("ethereum:{PAYEE}@56")),
        (BSC, format!("ethereum:{PAYEE}@0x38")),
        (POLYGON, format!("ethereum:{PAYEE}")),
        (POLYGON, format!("ethereum:{PAYEE}@137")),
        (POLYGON, format!("ethereum:pay-{PAYEE}@0x89?value=1e18")),
        (ETHEREUM, format!("ethereum:{USDT_CONTRACT}@1/transfer?address={PAYEE}&uint256=1000000")),
        (ETHEREUM, format!("  {PAYEE}\n")),
    ];
    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|(entry, uri)| match entry.validate_address(uri) {
            Ok(address) if address.value() == PAYEE => None,
            other => Some(format!("{uri}: {other:?}")),
        })
        .collect();
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn payment_uris_refuse_what_jelly_cannot_send() {
    let unsupported =
        |entry: &dyn ChainEntry| AddressError::UnsupportedRequest { chain: entry.spec().chain };
    let network = |entry: &dyn ChainEntry| AddressError::WrongNetwork { chain: entry.spec().chain };
    let cases = [
        (
            ETHEREUM,
            format!("ethereum:{USDT_CONTRACT}/approve?address={PAYEE}&uint256=1"),
            unsupported(ETHEREUM),
        ),
        (ETHEREUM, format!("ethereum:{USDT_CONTRACT}/transfer?uint256=1"), unsupported(ETHEREUM)),
        (ETHEREUM, String::from("ethereum:"), unsupported(ETHEREUM)),
        (ETHEREUM, format!("ethereum:{PAYEE}@56"), network(ETHEREUM)),
        (ETHEREUM, format!("ethereum:{PAYEE}@polygon"), network(ETHEREUM)),
        (BSC, format!("ethereum:{PAYEE}@1"), network(BSC)),
        (ETHEREUM, format!("ethereum:{PAYEE}@137"), network(ETHEREUM)),
        (POLYGON, format!("ethereum:{PAYEE}@1"), network(POLYGON)),
        (POLYGON, format!("ethereum:{PAYEE}@56"), network(POLYGON)),
    ];
    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|(entry, uri, want)| match entry.validate_address(uri) {
            Err(got) if got == *want => None,
            other => Some(format!("{uri}: {other:?}")),
        })
        .collect();
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn a_keystore_imports_the_documented_key_and_address() {
    let suite = keystore().unwrap();
    let password = SecretText::new(suite.password.clone());
    let want = ETHEREUM.codec().validate(&suite.address).unwrap();
    for document in [&suite.pbkdf2, &suite.scrypt] {
        assert_eq!(ETHEREUM.recognize_key(document), Some(KEYSTORE_FORMAT));
        let imported = ETHEREUM.import_key(document, Some(&password)).unwrap();
        assert_eq!(imported.named_script, None);
        for entry in ENTRIES {
            assert_eq!(entry.address(&imported.material, None).unwrap(), want.value());
        }
        assert_eq!(BSC.export_key(&imported.material).unwrap().as_str(), suite.secret);
    }
}

#[test]
fn a_keystore_is_named_and_asks_for_its_password_before_it_is_read() {
    let document = r#"{"crypto":{"cipher":"aes-128-ctr"},"version":3}"#;
    let password = SecretText::new(String::from("testpassword"));
    for entry in ENTRIES {
        assert_eq!(entry.recognize_key(document), Some(KEYSTORE_FORMAT));
        assert!(matches!(entry.import_key(document, None), Err(KeyError::PasswordRequired)));
        assert!(matches!(
            entry.import_key(document, Some(&password)),
            Err(KeyError::KeystoreMalformed)
        ));
    }
}

#[test]
fn a_keystore_asks_for_its_password_and_refuses_a_wrong_one() {
    let suite = keystore().unwrap();
    assert!(matches!(ETHEREUM.import_key(&suite.pbkdf2, None), Err(KeyError::PasswordRequired)));
    let wrong = SecretText::new(String::from("not the password"));
    assert!(matches!(
        ETHEREUM.import_key(&suite.pbkdf2, Some(&wrong)),
        Err(KeyError::WrongPassword)
    ));
}

#[test]
fn a_rejected_address_or_key_never_appears_in_its_error() {
    for entry in ENTRIES {
        let refused = entry.validate_address(MARKER_PHRASE).unwrap_err();
        assert_redacted("validate_address", MARKER_PHRASE, &KeyError::from(refused));
    }
    let refused = ETHEREUM.import_key(MARKER_PHRASE, None).unwrap_err();
    assert_redacted("import_key", MARKER_PHRASE, &refused);
    let hex_key = format!("0x{}", "ff".repeat(32));
    let refused = ETHEREUM.import_key(&hex_key, None).unwrap_err();
    assert_redacted("import_key", &hex_key, &refused);
}
