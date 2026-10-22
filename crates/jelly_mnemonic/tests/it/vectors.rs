//! The published BIP-39 English vectors and word list.

use bip39::Language;
use bitcoin::bip32::{Xpriv, Xpub};
use bitcoin::secp256k1::Secp256k1;
use jelly_keypair::{HdKeychain, parse_path};
use jelly_mnemonic::Mnemonic;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const VECTORS_JSON: &str = include_str!("../vectors/bip39_english.json");

#[derive(Deserialize)]
struct Vectors {
    wordlist_sha256: String,
    /// `[entropy, phrase, seed, xprv]` per row.
    vectors: Vec<[String; 4]>,
}

#[test]
fn phrases_and_seeds_match_the_published_trezor_vectors() {
    let suite: Vectors = serde_json::from_str(VECTORS_JSON).expect("the vector file parses");
    assert_eq!(suite.vectors.len(), 24, "every english vector is present");
    let master = parse_path("m").expect("the master path");
    for [entropy_hex, phrase, seed_hex, xprv] in &suite.vectors {
        let entropy = hex::decode(entropy_hex).expect("the vector entropy is hex");
        let from_entropy = Mnemonic::from_entropy(&entropy).expect("entropy to mnemonic");
        assert_eq!(from_entropy.phrase().as_str(), phrase);

        let parsed = Mnemonic::parse(phrase).expect("the phrase parses");
        assert_eq!(parsed.with_entropy(|bytes| hex::encode(bytes)), *entropy_hex);

        let seed = parsed.to_seed("TREZOR");
        assert_eq!(seed.with_bytes(|bytes| hex::encode(bytes)), *seed_hex);
        let keychain = HdKeychain::from_seed(seed);
        let expected = Xpub::from_priv(
            &Secp256k1::signing_only(),
            &xprv.parse::<Xpriv>().expect("the vector xprv parses"),
        );
        assert_eq!(keychain.xpub_at(&master).expect("the master xpub"), expected.to_string());
    }
}

#[test]
fn the_word_list_is_the_published_one() {
    let suite: Vectors = serde_json::from_str(VECTORS_JSON).expect("the vector file parses");
    let joined: String =
        Language::English.word_list().iter().flat_map(|word| [*word, "\n"]).collect();
    assert_eq!(hex::encode(Sha256::digest(joined.as_bytes())), suite.wordlist_sha256);
}
