use jelly_core::{
    ChainId, KeyError, KeyKind, ScriptKind,
    test_util::{
        MARKER, MARKER_PHRASE, assert_rejected_without_leak, key_material, key_material_of,
    },
};
use jelly_keypair::{
    PathTemplate, parse_path,
    test_util::{key_at, test_keychain, uncompressed_wif},
    wif_encode,
};
use jelly_litecoin::{ENTRY, SPEC};
use jelly_utxo::test_util::address_at;

/// The Litecoin WIF of `m/44'/2'/0'/0/0`, cross-checked against an independent
/// implementation.
const LITECOIN_WIF: &str = "T5b4RiWRs7XG8xZ2bCHBoJcn4JrpMTbGRFYXgoZHd7nD8izwqhMK";

/// BIP-84 publishes this Bitcoin WIF for `m/84'/0'/0'/0/0`.
const BITCOIN_WIF: &str = "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d";

const NAME: &str = "Litecoin WIF key";

#[test]
fn the_litecoin_wif_round_trips() {
    let litecoin_path = "m/44'/2'/0'/0/0";
    let exported = ENTRY.export_key(&key_at(litecoin_path).unwrap()).unwrap();
    assert_eq!(exported.as_str(), LITECOIN_WIF);

    let imported = ENTRY.import_key(LITECOIN_WIF, None).unwrap();
    assert_eq!(imported.named_script, None);
    let derived = test_keychain().secp256k1_at(&parse_path(litecoin_path).unwrap()).unwrap();
    imported.material.with_bytes(|bytes| {
        derived.with_secret_bytes(|secret| assert_eq!(bytes, secret.as_slice()));
    });
}

#[test]
fn an_exported_key_reimports_to_the_same_address() {
    let first = PathTemplate::parse(SPEC.default_path).unwrap().path_at(0).unwrap().to_string();
    let exported = ENTRY.export_key(&key_at(&first).unwrap()).unwrap();
    let reimported = ENTRY.import_key(&exported, None).unwrap();
    assert_eq!(
        ENTRY.address(&reimported.material, reimported.named_script).unwrap(),
        address_at(ENTRY, &first, None).unwrap()
    );
}

#[test]
fn the_compressed_flag_decides_whether_a_wif_imports() {
    let key = [7u8; 32];
    let compressed = wif_encode(&key, 0xb0);
    let imported = ENTRY.import_key(&compressed, None).unwrap();
    imported.material.with_bytes(|bytes| assert_eq!(bytes, key));
    assert_eq!(ENTRY.recognize_key(&compressed), Some(NAME));

    let uncompressed = uncompressed_wif(&key, 0xb0);
    assert!(matches!(ENTRY.import_key(&uncompressed, None), Err(KeyError::UncompressedKey)));
    assert_eq!(ENTRY.recognize_key(&uncompressed), Some(NAME));
}

#[test]
fn electrum_script_prefixes_name_the_address_type() {
    let key = [7u8; 32];
    let wif = wif_encode(&key, 0xb0);
    for (prefix, script) in [
        ("p2wpkh", ScriptKind::P2wpkh),
        ("p2wpkh-p2sh", ScriptKind::P2shP2wpkh),
        ("p2pkh", ScriptKind::P2pkh),
    ] {
        let input = format!("{prefix}:{}", wif.as_str());
        let imported = ENTRY.import_key(&input, None).unwrap();
        assert_eq!(imported.named_script, Some(script), "{script:?}");
        imported.material.with_bytes(|bytes| assert_eq!(bytes, key));
        assert_eq!(ENTRY.recognize_key(&input), Some(NAME));
    }
    // A taproot prefix imports, but Litecoin doesn't derive taproot, so the address is
    // refused.
    let taproot = ENTRY.import_key(&format!("p2tr:{}", wif.as_str()), None).unwrap();
    assert_eq!(taproot.named_script, Some(ScriptKind::P2tr));
    assert!(matches!(
        ENTRY.address(&taproot.material, taproot.named_script),
        Err(KeyError::ScriptUnsupported { chain: ChainId::Litecoin, script: ScriptKind::P2tr })
    ));
}

#[test]
fn electrum_3_version_byte_offsets_name_the_address_type() {
    let key = [7u8; 32];
    for (version, script) in [(0xb1, ScriptKind::P2wpkh), (0xb2, ScriptKind::P2shP2wpkh)] {
        let wif = wif_encode(&key, version);
        let imported = ENTRY.import_key(&wif, None).unwrap();
        assert_eq!(imported.named_script, Some(script), "{version:#x}");
        // Electrum refuses a prefix on a key whose version byte already names a type.
        let prefixed = format!("p2wpkh:{}", wif.as_str());
        assert!(matches!(ENTRY.import_key(&prefixed, None), Err(KeyError::UnknownFormat)));
        assert_eq!(ENTRY.recognize_key(&prefixed), None);
    }
}

/// Another chain's WIF isn't recognized here, so the chain table can name the chain it
/// belongs to.
#[test]
fn a_version_byte_names_only_its_own_network() {
    let key = [7u8; 32];
    for version in [0x00, 0x80, 0x81, 0x9e, 0xb3] {
        let wif = wif_encode(&key, version);
        assert!(
            matches!(ENTRY.import_key(&wif, None), Err(KeyError::UnknownFormat)),
            "{version:#x}"
        );
        assert_eq!(ENTRY.recognize_key(&wif), None, "{version:#x}");
    }
    for input in [BITCOIN_WIF.to_owned(), format!("p2wpkh:{BITCOIN_WIF}")] {
        assert!(matches!(ENTRY.import_key(&input, None), Err(KeyError::UnknownFormat)), "{input}");
        assert_eq!(ENTRY.recognize_key(&input), None, "{input}");
    }
}

#[test]
fn a_corrupted_wif_is_refused() {
    let mut corrupted = String::from(LITECOIN_WIF);
    corrupted.pop();
    corrupted.push('X');
    assert!(matches!(ENTRY.import_key(&corrupted, None), Err(KeyError::UnknownFormat)));
    assert_eq!(ENTRY.recognize_key(&corrupted), None);
}

#[test]
fn input_in_no_key_format_is_refused() {
    for input in ["", "   ", "0x1234", "hello world there", "[1, 2, 3]"] {
        assert!(matches!(ENTRY.import_key(input, None), Err(KeyError::UnknownFormat)), "{input:?}");
        assert_eq!(ENTRY.recognize_key(input), None, "{input:?}");
    }
}

#[test]
fn bare_hex_is_accepted() {
    let hex_key = "1ab42cc412b618bdea3a599e3c9bae199ebf030895b039e9db1e30dafb12b727";
    let expected = ENTRY
        .address(&key_material_of(KeyKind::Secp256k1, hex::decode(hex_key).unwrap()), None)
        .unwrap();
    let imported = ENTRY.import_key(hex_key, None).unwrap();
    assert_eq!(ENTRY.address(&imported.material, None).unwrap(), expected);
    assert_eq!(ENTRY.recognize_key(hex_key), Some("32-byte hex key"));
}

#[test]
fn a_key_of_another_curve_is_refused() {
    let ed25519 = key_material(KeyKind::Ed25519Seed, 7);
    assert!(matches!(
        ENTRY.export_key(&ed25519),
        Err(KeyError::WrongKeyKind { chain: ChainId::Litecoin })
    ));
    assert!(matches!(
        ENTRY.address(&ed25519, None),
        Err(KeyError::WrongKeyKind { chain: ChainId::Litecoin })
    ));
}

#[test]
fn a_rejected_key_never_appears_in_its_error() {
    assert_rejected_without_leak("import_key", MARKER, ENTRY.import_key(MARKER, None));
    assert_rejected_without_leak(
        "import_key",
        MARKER_PHRASE,
        ENTRY.import_key(MARKER_PHRASE, None),
    );
}

#[test]
fn an_imported_key_redacts_its_debug() {
    let imported = ENTRY.import_key(LITECOIN_WIF, None).unwrap();
    let debug = format!("{imported:?}");
    imported.material.with_bytes(|bytes| assert!(!debug.contains(&hex::encode(bytes))));
    assert!(!debug.contains(LITECOIN_WIF));
}
