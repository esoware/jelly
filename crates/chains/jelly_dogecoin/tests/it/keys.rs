use jelly_core::{
    ChainId, KeyError, KeyKind, ScriptKind,
    test_util::{
        MARKER, MARKER_PHRASE, assert_rejected_without_leak, key_material, key_material_of,
    },
};
use jelly_dogecoin::{ENTRY, SPEC};
use jelly_keypair::{
    PathTemplate,
    test_util::{key_at, uncompressed_wif},
    wif_encode,
};
use jelly_utxo::test_util::address_at;

/// The Dogecoin WIF of `m/44'/3'/0'/0/0`, reproduced by `bip_utils`, an independent
/// implementation.
const DOGECOIN_WIF: &str = "QPkeC1ZfHx3c9g7WTj9cQ8gnvk2iSAfAcbq1aVAWjNTwDAKfZUzx";

/// BIP-84 publishes this Bitcoin WIF for `m/84'/0'/0'/0/0`.
const BITCOIN_WIF: &str = "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d";

const NAME: &str = "Dogecoin WIF key";

#[test]
fn the_dogecoin_wif_round_trips() {
    let exported = ENTRY.export_key(&key_at("m/44'/3'/0'/0/0").unwrap()).unwrap();
    assert_eq!(exported.as_str(), DOGECOIN_WIF);
    let imported = ENTRY.import_key(DOGECOIN_WIF, None).unwrap();
    assert_eq!(imported.named_script, None);
    assert_eq!(
        ENTRY.address(&imported.material, None).unwrap(),
        address_at(ENTRY, "m/44'/3'/0'/0/0", None).unwrap()
    );
    assert_eq!(ENTRY.recognize_key(DOGECOIN_WIF), Some(NAME));
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
    let compressed = wif_encode(&key, 0x9e);
    let imported = ENTRY.import_key(&compressed, None).unwrap();
    imported.material.with_bytes(|bytes| assert_eq!(bytes, key));
    assert_eq!(ENTRY.recognize_key(&compressed), Some(NAME));

    let uncompressed = uncompressed_wif(&key, 0x9e);
    assert!(matches!(ENTRY.import_key(&uncompressed, None), Err(KeyError::UncompressedKey)));
    assert_eq!(ENTRY.recognize_key(&uncompressed), Some(NAME));
}

/// Without segwit, the bytes just above Dogecoin's WIF version name no script type.
#[test]
fn dogecoins_version_byte_carries_no_script_offset() {
    let key = [7u8; 32];
    for version in [0x9f, 0xa0] {
        let wif = wif_encode(&key, version);
        assert!(
            matches!(ENTRY.import_key(&wif, None), Err(KeyError::UnknownFormat)),
            "{version:#x}"
        );
        assert_eq!(ENTRY.recognize_key(&wif), None, "{version:#x}");
    }
}

#[test]
fn an_electrum_prefix_names_a_flavor_the_chain_then_resolves() {
    let wif = wif_encode(&[7u8; 32], 0x9e);
    let legacy = ENTRY.import_key(&format!("p2pkh:{}", wif.as_str()), None).unwrap();
    assert_eq!(legacy.named_script, Some(ScriptKind::P2pkh));
    ENTRY.address(&legacy.material, legacy.named_script).unwrap();

    let segwit = ENTRY.import_key(&format!("p2wpkh:{}", wif.as_str()), None).unwrap();
    assert!(matches!(
        ENTRY.address(&segwit.material, segwit.named_script),
        Err(KeyError::ScriptUnsupported { chain: ChainId::Dogecoin, script: ScriptKind::P2wpkh })
    ));
}

/// Another chain's WIF isn't recognized here, so the chain table can name the chain it
/// belongs to.
#[test]
fn a_version_byte_names_only_its_own_network() {
    let key = [7u8; 32];
    for version in [0x00, 0x80, 0xb0] {
        let wif = wif_encode(&key, version);
        assert!(
            matches!(ENTRY.import_key(&wif, None), Err(KeyError::UnknownFormat)),
            "{version:#x}"
        );
        assert_eq!(ENTRY.recognize_key(&wif), None, "{version:#x}");
    }
    assert!(matches!(ENTRY.import_key(BITCOIN_WIF, None), Err(KeyError::UnknownFormat)));
    assert_eq!(ENTRY.recognize_key(BITCOIN_WIF), None);
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
        Err(KeyError::WrongKeyKind { chain: ChainId::Dogecoin })
    ));
    assert!(matches!(
        ENTRY.address(&ed25519, None),
        Err(KeyError::WrongKeyKind { chain: ChainId::Dogecoin })
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
    let imported = ENTRY.import_key(DOGECOIN_WIF, None).unwrap();
    let debug = format!("{imported:?}");
    imported.material.with_bytes(|bytes| assert!(!debug.contains(&hex::encode(bytes))));
    assert!(!debug.contains(DOGECOIN_WIF));
}
