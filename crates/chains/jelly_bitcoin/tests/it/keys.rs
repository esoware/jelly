use bitcoin::{Address, Network, PrivateKey};
use jelly_bitcoin::{ENTRY, SPEC};
use jelly_core::{
    ChainId, KeyError, KeyKind, ScriptKind,
    test_util::{
        MARKER, MARKER_PHRASE, assert_rejected_without_leak, key_material, key_material_of,
    },
};
use jelly_keypair::{
    PathTemplate, signing_context,
    test_util::{key_at, uncompressed_wif},
    wif_encode,
};
use jelly_utxo::test_util::address_at;

/// BIP-84 publishes the WIF for `m/84'/0'/0'/0/0` next to the address.
const BIP84_WIF: &str = "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d";
const BIP84_ADDRESS: &str = "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu";

/// The secret scalar one as an uncompressed WIF, and the P2PKH address of its
/// uncompressed public key, which is where a legacy wallet holding it was paid.
const UNCOMPRESSED_WIF: &str = "5HpHagT65TZzG1PH3CSu63k8DbpvD8s5ip4nEB3kEsreAnchuDf";
const UNCOMPRESSED_P2PKH: &str = "1EHNa6Q4Jz2uvNExL497mE43ikXhwF6kZm";

const NAME: &str = "Bitcoin WIF key";

#[test]
fn the_bitcoin_wif_round_trips() {
    let exported = ENTRY.export_key(&key_at("m/84'/0'/0'/0/0").unwrap()).unwrap();
    assert_eq!(exported.as_str(), BIP84_WIF);

    let imported = ENTRY.import_key(BIP84_WIF, None).unwrap();
    assert_eq!(imported.named_script, None);
    assert_eq!(ENTRY.address(&imported.material, Some(ScriptKind::P2wpkh)).unwrap(), BIP84_ADDRESS);
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
    let compressed = wif_encode(&key, 0x80);
    let imported = ENTRY.import_key(&compressed, None).unwrap();
    imported.material.with_bytes(|bytes| assert_eq!(bytes, key));
    assert_eq!(ENTRY.recognize_key(&compressed), Some(NAME));

    // A key's addresses all come from its compressed public key, and an uncompressed
    // WIF's funds sit at the uncompressed key's address.
    let uncompressed = uncompressed_wif(&key, 0x80);
    assert!(matches!(ENTRY.import_key(&uncompressed, None), Err(KeyError::UncompressedKey)));
    assert_eq!(ENTRY.recognize_key(&uncompressed), Some(NAME));
}

#[test]
fn an_uncompressed_wif_never_shows_the_compressed_keys_address() {
    let oracle = PrivateKey::from_wif(UNCOMPRESSED_WIF).unwrap();
    assert!(!oracle.compressed, "the fixture is an uncompressed wif");
    assert_eq!(
        Address::p2pkh(oracle.public_key(signing_context()), Network::Bitcoin).to_string(),
        UNCOMPRESSED_P2PKH
    );
    // Refusing the key is also correct. Showing a different address is not.
    if let Ok(imported) = ENTRY.import_key(UNCOMPRESSED_WIF, None) {
        assert_eq!(
            ENTRY.address(&imported.material, Some(ScriptKind::P2pkh)).unwrap(),
            UNCOMPRESSED_P2PKH
        );
    }
}

#[test]
fn electrum_script_prefixes_name_the_address_type() {
    let key = [7u8; 32];
    let wif = wif_encode(&key, 0x80);
    for (input, script) in [
        (format!("p2pkh:{}", wif.as_str()), ScriptKind::P2pkh),
        (format!(" P2WPKH-P2SH: {} ", wif.as_str()), ScriptKind::P2shP2wpkh),
        (format!("p2tr:{}", wif.as_str()), ScriptKind::P2tr),
        (format!("p2wpkh:{}", wif.as_str()), ScriptKind::P2wpkh),
    ] {
        let imported = ENTRY.import_key(&input, None).unwrap();
        assert_eq!(imported.named_script, Some(script), "{script:?}");
        imported.material.with_bytes(|bytes| assert_eq!(bytes, key));
        assert_eq!(ENTRY.recognize_key(&input), Some(NAME));
    }

    let prefixed = ENTRY.import_key(&format!("p2wpkh:{BIP84_WIF}"), None).unwrap();
    assert_eq!(ENTRY.address(&prefixed.material, prefixed.named_script).unwrap(), BIP84_ADDRESS);

    let hex_key = hex::encode(key);
    for input in [
        format!("p2wsh:{}", wif.as_str()),
        format!("key:{}", wif.as_str()),
        format!("p2wpkh:{hex_key}"),
        String::from("p2wpkh:"),
    ] {
        assert!(matches!(ENTRY.import_key(&input, None), Err(KeyError::UnknownFormat)), "{input}");
        assert_eq!(ENTRY.recognize_key(&input), None, "{input}");
    }
}

#[test]
fn electrum_3_version_byte_offsets_name_the_address_type() {
    let key = [7u8; 32];
    for (version, script) in [(0x81, ScriptKind::P2wpkh), (0x82, ScriptKind::P2shP2wpkh)] {
        let wif = wif_encode(&key, version);
        let imported = ENTRY.import_key(&wif, None).unwrap();
        assert_eq!(imported.named_script, Some(script), "{version:#x}");
        assert_eq!(ENTRY.recognize_key(&wif), Some(NAME));
        // Electrum refuses a prefix on a key whose version byte already names a type.
        let prefixed = format!("p2wpkh:{}", wif.as_str());
        assert!(matches!(ENTRY.import_key(&prefixed, None), Err(KeyError::UnknownFormat)));
        assert_eq!(ENTRY.recognize_key(&prefixed), None);
    }
}

#[test]
fn a_version_byte_names_only_its_own_network() {
    let key = [7u8; 32];
    // Bitcoin's P2PKH address byte, Electrum's multisig offset, and Litecoin's and
    // Dogecoin's WIF bytes.
    for version in [0x00, 0x83, 0xb0, 0x9e] {
        let wif = wif_encode(&key, version);
        assert!(
            matches!(ENTRY.import_key(&wif, None), Err(KeyError::UnknownFormat)),
            "{version:#x}"
        );
        assert_eq!(ENTRY.recognize_key(&wif), None, "{version:#x}");
    }
}

#[test]
fn a_corrupted_wif_is_refused() {
    let mut corrupted = String::from(BIP84_WIF);
    corrupted.pop();
    corrupted.push('X');
    assert!(matches!(ENTRY.import_key(&corrupted, None), Err(KeyError::UnknownFormat)));
    assert_eq!(ENTRY.recognize_key(&corrupted), None);
}

#[test]
fn input_in_no_key_format_is_refused() {
    for input in ["", "   ", "0x1234", "hello world there", "not base58 !!!", "[1, 2, 3]"] {
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
    for input in [hex_key.to_owned(), format!("0x{}", hex_key.to_uppercase())] {
        let imported = ENTRY.import_key(&input, None).unwrap();
        assert_eq!(imported.named_script, None);
        assert_eq!(ENTRY.address(&imported.material, None).unwrap(), expected);
        assert_eq!(ENTRY.recognize_key(&input), Some("32-byte hex key"));
    }
    assert!(matches!(ENTRY.import_key(&"ff".repeat(32), None), Err(KeyError::InvalidSecretKey)));
}

#[test]
fn a_key_of_another_curve_is_refused() {
    let ed25519 = key_material(KeyKind::Ed25519Seed, 7);
    assert!(matches!(
        ENTRY.export_key(&ed25519),
        Err(KeyError::WrongKeyKind { chain: ChainId::Bitcoin })
    ));
    assert!(matches!(
        ENTRY.address(&ed25519, None),
        Err(KeyError::WrongKeyKind { chain: ChainId::Bitcoin })
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
    let hex_key = format!("0x{}", "ff".repeat(32));
    assert_rejected_without_leak("import_key", &hex_key, ENTRY.import_key(&hex_key, None));
}

#[test]
fn an_imported_key_redacts_its_debug() {
    let imported = ENTRY.import_key(BIP84_WIF, None).unwrap();
    let debug = format!("{imported:?}");
    imported.material.with_bytes(|bytes| assert!(!debug.contains(&hex::encode(bytes))));
    assert!(!debug.contains(BIP84_WIF));
}
