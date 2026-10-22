use std::collections::HashSet;
use std::error::Error;

use base64::prelude::{BASE64_STANDARD, Engine as _};
use jelly_core::KeyKind;
use jelly_vault::{BlobContent, SecretBlob, test_util::TempVault};
use zeroize::Zeroizing;

use crate::common::{WALLET_ID, blob};

fn field(fixture: &TempVault, pointer: &str) -> Result<String, Box<dyn Error>> {
    let forge = fixture.forge()?;
    Ok(forge.string(pointer).ok_or("the field is not a string")?.to_owned())
}

#[test]
fn every_write_draws_a_fresh_nonce() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let mut seen = HashSet::new();
    assert!(seen.insert(field(&fixture, "/aead/nonce").unwrap()));

    for i in 0..5u8 {
        let id = vault.insert(blob(BlobContent::MoneroScanState, i, 8)).expect("insert");
        assert!(seen.insert(field(&fixture, "/aead/nonce").unwrap()), "a nonce was drawn twice");
        vault.remove(id).expect("remove");
        assert!(seen.insert(field(&fixture, "/aead/nonce").unwrap()), "a nonce was drawn twice");
    }
    assert_eq!(seen.len(), 11);
}

#[test]
fn the_salts_and_commitment_are_stable_across_edits() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let kdf_salt = field(&fixture, "/kdf/salt").unwrap();
    let hkdf_salt = field(&fixture, "/hkdf/salt").unwrap();
    let commitment = field(&fixture, "/commitment").unwrap();

    vault.insert(blob(BlobContent::SeedEntropy, 1, 32)).expect("insert");
    assert_eq!(field(&fixture, "/kdf/salt").unwrap(), kdf_salt);
    assert_eq!(field(&fixture, "/hkdf/salt").unwrap(), hkdf_salt);
    assert_eq!(field(&fixture, "/commitment").unwrap(), commitment);
}

#[test]
fn the_ciphertext_holds_none_of_the_plaintext() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let secret = *b"jelly-envelope-distinctive-bytes";
    vault
        .insert(SecretBlob::new(
            BlobContent::PrivateKey(KeyKind::Ed25519Seed),
            Zeroizing::new(secret.to_vec()),
        ))
        .expect("insert");
    vault.lock();

    let bytes = fixture.read_file().expect("read");
    let text = str::from_utf8(&bytes).expect("the vault file is utf-8 json");
    assert!(!text.contains(&hex::encode(secret)), "hex form must not appear");
    assert!(!text.contains(&hex::encode_upper(secret)), "upper hex form must not appear");
    assert!(!text.contains(&BASE64_STANDARD.encode(secret)), "base64 form must not appear");
    assert!(
        !bytes.windows(secret.len()).any(|window| window == secret),
        "raw bytes must not appear"
    );
    for label in ["private_key", "ed25519_seed", "seed_entropy"] {
        assert!(!text.contains(label), "the payload label {label} must not appear in the file");
    }
}

#[test]
fn removing_an_entry_shrinks_the_file() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let id = vault.insert(blob(BlobContent::SeedEntropy, 7, 4096)).expect("insert");
    let big = fixture.read_file().expect("read").len();
    vault.remove(id).expect("remove");
    let small = fixture.read_file().expect("read").len();
    assert!(small < big, "{small} is not smaller than {big}");
}
