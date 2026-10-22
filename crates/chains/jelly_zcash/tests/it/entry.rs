use jelly_core::{
    AddressError, ChainId, KeyError, KeyKind, KeyMaterial, ScriptKind,
    test_util::{MARKER, MARKER_PHRASE, assert_redacted},
};
use jelly_keypair::{
    PathTemplate, Secp256k1Key, generate_key, parse_path,
    test_util::{test_keychain, uncompressed_wif},
    wif_encode,
};
use jelly_zcash::{ENTRY, SPEC};
use zeroize::Zeroizing;

use crate::common::{CHAIN, OWNER, RECIPIENT};

const SECRET: &str = "2d8f68944bdbfbc0769542fba8fc2d2a3de67393334471624364c7006da2aa54";

/// BIP-84 publishes the WIF for `m/84'/0'/0'/0/0` of the all-abandon phrase. Zcash kept
/// Bitcoin's version byte, so it is a Zcash key too.
const BIP84_WIF: &str = "KyZpNDKnfs94vbrwhJneDi77V6jF64PWPF8x5cdJb8ifgg2DUc9d";

const P2PKH: Option<ScriptKind> = Some(ScriptKind::P2pkh);

#[test]
fn the_entry_serves_its_own_spec_codec_and_endpoints() {
    assert_eq!(SPEC.chain, CHAIN);
    assert_eq!(ENTRY.spec(), &SPEC);
    assert_eq!(ENTRY.codec().chain(), CHAIN);
    assert_eq!(SPEC.transports.first(), Some(&SPEC.primary_transport));
    assert_eq!(SPEC.resolve_script(None, None).unwrap(), P2PKH);
    assert!(ENTRY.default_endpoints().contains("\"chain\": \"zcash\""));
}

#[test]
fn trust_wallets_key_vector_pays_its_p2pkh_address_and_round_trips_through_a_wif() {
    let key = ENTRY.import_key(SECRET, None).unwrap();
    assert_eq!(key.named_script, None);
    assert_eq!(ENTRY.address(&key.material, P2PKH).unwrap(), OWNER);
    let exported = ENTRY.export_key(&key.material).unwrap();
    assert!(exported.starts_with(['K', 'L']));
    let imported = ENTRY.import_key(&exported, None).unwrap();
    assert_eq!(ENTRY.address(&imported.material, P2PKH).unwrap(), OWNER);
    for kind in [ScriptKind::P2tr, ScriptKind::P2wpkh, ScriptKind::P2shP2wpkh] {
        assert!(matches!(
            SPEC.resolve_script(None, Some(kind)),
            Err(KeyError::ScriptUnsupported { chain: ChainId::Zcash, .. })
        ));
        assert!(matches!(
            ENTRY.address(&key.material, Some(kind)),
            Err(KeyError::ScriptUnsupported { chain: ChainId::Zcash, .. })
        ));
    }
    assert_eq!(
        PathTemplate::parse(SPEC.default_path).unwrap().path_at(0).unwrap().to_string(),
        "44'/133'/0'/0/0"
    );
}

#[test]
fn transparent_p2pkh_p2sh_and_tex_have_the_right_scripts() {
    let codec = ENTRY.codec();
    let script = codec.script_pubkey(OWNER).unwrap();
    assert_eq!(hex::encode(&script), "76a9148c6f453157897ce2e6de413f329d995fe0d8f90288ac");
    assert_eq!(codec.address_of(&script).as_deref(), Some(OWNER));
    assert_eq!(codec.script_kind(OWNER), Some(ScriptKind::P2pkh));
    let multisig = "t3RD6RFKhWSotNbPEY4Vw7Ku9QCfKkzrbBL";
    let script = codec.script_pubkey(multisig).unwrap();
    assert_eq!(script.len(), 23);
    assert_eq!(codec.address_of(&script).as_deref(), Some(multisig));
    assert_eq!(codec.script_kind(multisig), None);
    let tex = "tex1s2rt77ggv6q989lr49rkgzmh5slsksa9khdgte";
    assert_eq!(ENTRY.validate_address(tex).unwrap().value(), RECIPIENT);
    assert_eq!(ENTRY.validate_address(&tex.to_uppercase()).unwrap().value(), RECIPIENT);
    assert_eq!(codec.script_pubkey(tex).unwrap(), codec.script_pubkey(RECIPIENT).unwrap());
    assert_eq!(codec.address_of(&[0x6a, 0x01, 0x00]), None);
}

#[test]
fn only_transparent_mainnet_addresses_and_requests_without_unknown_parameters_are_read() {
    for value in [
        "",
        "   ",
        "175tWpb8K1S7NmH4Zx6rewF9WQrcZv245W",
        "t1RygJmrLdNGgi98gUgEJDTVaELTAYWoMBz",
        "zs1shielded",
        "zcSprout",
        "u1unified",
        "textest1s2rt77ggv6q989lr49rkgzmh5slsksa9khdgte",
    ] {
        assert!(ENTRY.validate_address(value).is_err(), "{value}");
    }
    for uri in [
        format!("zcash:{OWNER}?amount=1.25&label=Payment"),
        format!("ZCASH:{OWNER}?message=Thanks"),
        format!("zcash:?address={OWNER}&amount=1.25"),
        format!("  {OWNER}\n"),
    ] {
        assert_eq!(ENTRY.validate_address(&uri).unwrap().value(), OWNER, "{uri}");
    }
    for query in [
        "memo=AAAA",
        "memo.1=AAAA",
        "memo%2E1=AAAA",
        "address.1=unused",
        "req-memo=AAAA",
        "address=duplicate",
    ] {
        assert_eq!(
            ENTRY.validate_address(&format!("zcash:{OWNER}?{query}")),
            Err(AddressError::UnsupportedRequest { chain: CHAIN }),
            "{query}"
        );
    }
    for uri in ["zcash:", "zcash:?amount=1", "zcash:?address=", "zcash:?address=a&address=b"] {
        assert_eq!(
            ENTRY.validate_address(uri),
            Err(AddressError::UnsupportedRequest { chain: CHAIN }),
            "{uri}"
        );
    }
    ENTRY.validate_address(&format!("bitcoin:{OWNER}")).unwrap_err();
}

#[test]
fn an_uppercase_shielded_address_is_refused_as_unsupported_like_its_lowercase_spelling() {
    let sapling = "zs10lpewlejk9lvdnnx4jwf7vssstcag2whqxj8e6s38kdwu6xtsh85d25fhqcrt6fz9wpxvsw0nsl";
    let unsupported = AddressError::UnsupportedRequest { chain: CHAIN };
    assert_eq!(
        ENTRY.validate_address(sapling),
        Err(unsupported),
        "the lowercase spelling is refused as shielded"
    );
    let refused = ENTRY.validate_address(&sapling.to_uppercase());
    assert_eq!(
        refused,
        Err(unsupported),
        "expected the uppercase spelling bech32 allows to be refused as shielded, like the \
         lowercase one, got {refused:?}"
    );
}

#[test]
fn exported_keys_reimport_to_the_same_address() {
    let keychain = test_keychain();
    let template = PathTemplate::parse(SPEC.default_path).unwrap();
    for index in [0, 1, 7] {
        let material = keychain.key_material_at(SPEC.key_kind, &template.path_at(index).unwrap());
        let material = material.unwrap();
        assert_eq!(material.kind(), KeyKind::Secp256k1);
        let address = ENTRY.address(&material, P2PKH).unwrap();
        assert_eq!(ENTRY.validate_address(&address).unwrap().value(), address);
        let exported = ENTRY.export_key(&material).unwrap();
        let reimported = ENTRY.import_key(&exported, None).unwrap();
        assert_eq!(ENTRY.address(&reimported.material, P2PKH).unwrap(), address, "{index}");
    }
}

#[test]
fn a_generated_key_produces_a_p2pkh_address_the_codec_accepts() {
    let key = generate_key(SPEC.key_kind).unwrap();
    assert_eq!(key.kind(), KeyKind::Secp256k1);
    let address = ENTRY.address(&key, P2PKH).unwrap();
    assert_eq!(ENTRY.validate_address(&address).unwrap().value(), address);
    assert_eq!(ENTRY.codec().script_kind(&address), P2PKH);
}

#[test]
fn a_bitcoin_wif_and_bare_hex_import_and_name_their_formats() {
    let from_wif = ENTRY.import_key(BIP84_WIF, None).unwrap();
    assert_eq!(ENTRY.recognize_key(BIP84_WIF), Some("Bitcoin WIF key"));
    let derived = test_keychain().secp256k1_at(&parse_path("m/84'/0'/0'/0/0").unwrap()).unwrap();
    let secret = derived.with_secret_bytes(|bytes| hex::encode(bytes));
    from_wif.material.with_bytes(|bytes| assert_eq!(hex::encode(bytes), secret));
    for input in [secret.clone(), format!(" 0x{} ", secret.to_uppercase())] {
        assert_eq!(ENTRY.recognize_key(&input), Some("32-byte hex key"));
        let from_hex = ENTRY.import_key(&input, None).unwrap();
        assert_eq!(ENTRY.export_key(&from_hex.material).unwrap().as_str(), BIP84_WIF);
    }
}

#[test]
fn an_uncompressed_wif_is_refused_and_another_networks_wif_is_not_read() {
    let secret = [7u8; 32];
    let uncompressed = uncompressed_wif(&secret, 0x80);
    assert!(matches!(ENTRY.import_key(&uncompressed, None), Err(KeyError::UncompressedKey)));
    assert_eq!(ENTRY.recognize_key(&uncompressed), Some("Bitcoin WIF key"));
    // Litecoin's, Dogecoin's, and Electrum 3.0's segwit version bytes name another
    // network's key, which the table attributes to the chain that reads it.
    for version in [0xb0, 0x9e, 0x81, 0x82] {
        let wif = wif_encode(&secret, version);
        assert!(
            matches!(ENTRY.import_key(&wif, None), Err(KeyError::UnknownFormat)),
            "{version:#x}"
        );
        assert_eq!(ENTRY.recognize_key(&wif), None, "{version:#x}");
    }
    let mut corrupted = String::from(BIP84_WIF);
    corrupted.pop();
    corrupted.push('X');
    assert!(matches!(ENTRY.import_key(&corrupted, None), Err(KeyError::UnknownFormat)));
    assert_eq!(ENTRY.recognize_key(&corrupted), None);
}

#[test]
fn input_in_no_key_format_is_refused() {
    for input in ["", "   ", "0x1234", "hello world there", MARKER] {
        assert!(matches!(ENTRY.import_key(input, None), Err(KeyError::UnknownFormat)), "{input:?}");
        assert_eq!(ENTRY.recognize_key(input), None, "{input:?}");
    }
    assert!(matches!(ENTRY.import_key(&"ff".repeat(32), None), Err(KeyError::InvalidSecretKey)));
}

#[test]
fn key_material_of_another_kind_is_refused() {
    let other = Secp256k1Key::from_secret_bytes(&[7; 32]).unwrap().key_material();
    let monero = KeyMaterial::new(
        KeyKind::MoneroSpend,
        other.with_bytes(|bytes| Zeroizing::new(bytes.to_vec())),
    );
    ENTRY.address(&monero, P2PKH).unwrap_err();
    ENTRY.export_key(&monero).unwrap_err();
    assert!(matches!(
        ENTRY.subaddress(&other, 0, 1),
        Err(KeyError::NoSubaddresses { chain: ChainId::Zcash })
    ));
}

#[test]
fn a_rejected_key_or_address_never_appears_in_its_error() {
    for input in [MARKER, MARKER_PHRASE] {
        let refused = ENTRY.import_key(input, None).expect_err("not a key");
        assert_redacted("import_key", input, &refused);
        let refused = ENTRY.validate_address(input).expect_err("not an address");
        assert_redacted("validate_address", input, &KeyError::from(refused));
    }
}
