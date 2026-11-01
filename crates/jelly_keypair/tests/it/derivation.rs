use bitcoin::bip32;
use curve25519_dalek::Scalar;
use jelly_core::{KeyError, KeyKind};
use jelly_keypair::{
    Secp256k1Key, parse_path, signing_context,
    test_util::{BIP84_WIF, key_at, test_seed},
    wif_encode,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct PublishedKeys {
    account_xprv_path: String,
    account_xprv: String,
    account_xpub: String,
    bip84_pubkey_path: String,
    bip84_pubkey: String,
    bip86_x_only_path: String,
    bip86_x_only: String,
}

#[test]
fn the_seed_derives_the_published_bip84_and_bip86_keys() {
    let suite: PublishedKeys =
        serde_json::from_str(include_str!("../vectors/bip84_bip86.json")).unwrap();
    let seed = test_seed();
    let account_xprv: bip32::Xpriv = suite.account_xprv.parse().unwrap();
    assert_eq!(
        bip32::Xpub::from_priv(signing_context(), &account_xprv).to_string(),
        suite.account_xpub
    );
    assert_eq!(
        seed.xpub_at(&parse_path(&suite.account_xprv_path).unwrap()).unwrap(),
        suite.account_xpub
    );

    let bip84 = seed.secp256k1_at(&parse_path(&suite.bip84_pubkey_path).unwrap()).unwrap();
    assert_eq!(hex::encode(bip84.public_compressed()), suite.bip84_pubkey);
    let bip86 = seed.secp256k1_at(&parse_path(&suite.bip86_x_only_path).unwrap()).unwrap();
    assert_eq!(hex::encode(&bip86.public_compressed()[1..]), suite.bip86_x_only);
}

#[test]
fn the_seed_derives_the_published_bip84_wif() {
    let key = Secp256k1Key::from_key_material(&key_at("m/84'/0'/0'/0/0").unwrap()).unwrap();
    let wif = key.with_secret_bytes(|secret| wif_encode(secret, 0x80));
    assert_eq!(wif.as_str(), BIP84_WIF);
}

#[test]
fn each_kind_derives_thirty_two_bytes_of_its_own_scheme() {
    let seed = test_seed();
    let secp256k1 =
        seed.key_material_at(KeyKind::Secp256k1, &parse_path("m/44'/0'/0'/0/0").unwrap()).unwrap();
    let ed25519 = seed
        .key_material_at(KeyKind::Ed25519Seed, &parse_path("m/44'/501'/0'/0'").unwrap())
        .unwrap();
    let monero =
        seed.key_material_at(KeyKind::MoneroSpend, &parse_path("m/44'/128'/0'").unwrap()).unwrap();
    for (material, kind) in [
        (&secp256k1, KeyKind::Secp256k1),
        (&ed25519, KeyKind::Ed25519Seed),
        (&monero, KeyKind::MoneroSpend),
    ] {
        assert_eq!(material.kind(), kind);
        assert_eq!(material.len(), 32);
    }
    // The spend key is already a scalar, so reducing it again changes nothing.
    monero.with_bytes(|bytes| {
        let again = Scalar::from_bytes_mod_order(bytes.try_into().unwrap());
        assert_eq!(again.as_bytes().as_slice(), bytes);
    });
}

#[test]
fn custom_paths_are_accepted_on_every_curve() {
    let seed = test_seed();
    for (kind, text) in [
        (KeyKind::Secp256k1, "m/1'/2/3'/4/5/6/7"),
        (KeyKind::Secp256k1, "m/44'/60'/9'/1/999"),
        (KeyKind::Ed25519Seed, "m/1'/2'/3'"),
        (KeyKind::MoneroSpend, "m/9'"),
        (KeyKind::Secp256k1, "m"),
    ] {
        let custom = seed.key_material_at(kind, &parse_path(text).unwrap()).expect("a key");
        let reference =
            seed.key_material_at(kind, &parse_path("m/44'/0'/0'").unwrap()).expect("a key");
        custom.with_bytes(|a| reference.with_bytes(|b| assert_ne!(a, b, "{kind:?} {text}")));
    }
}

#[test]
fn only_secp256k1_derives_through_a_normal_component() {
    let soft = parse_path("m/44'/501'/0'/0").unwrap();
    let seed = test_seed();
    for kind in [KeyKind::Ed25519Seed, KeyKind::MoneroSpend] {
        assert!(
            matches!(
                seed.key_material_at(kind, &soft),
                Err(KeyError::PathNotHardened { component: 3 })
            ),
            "{kind:?}"
        );
    }
    assert!(matches!(
        seed.key_material_at(KeyKind::Ed25519Seed, &parse_path("m/44'/607'/0").unwrap()),
        Err(KeyError::PathNotHardened { component: 2 })
    ));
    seed.key_material_at(KeyKind::Secp256k1, &soft).expect("a secp256k1 key");
}
