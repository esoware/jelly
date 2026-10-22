use std::sync::Arc;

use jelly_vault::{
    Argon2Params, BlobContent, MasterKey, MasterKeyStore as _, Vault, VaultError,
    test_util::{FailingKeyStore, Forge, TEST_PASSWORD, TempVault, password},
};
use serde_json::Value;

use crate::common::{WALLET_ID, blob, filled};

/// A named edit to one field of the file on disk, reporting whether the field
/// was there to edit.
type Mutation = (&'static str, fn(&mut Forge) -> bool);

#[test]
fn tampering_with_any_field_never_reads_as_a_wrong_password() {
    let cases: [Mutation; 13] = [
        ("magic", |document| document.set("/magic", Value::from("jelly-vault-x"))),
        ("wallet_id", |document| document.set("/wallet_id", Value::from("some-other-wallet"))),
        ("kdf.algorithm", |document| document.set("/kdf/algorithm", Value::from("argon2i"))),
        ("kdf.version", |document| document.set("/kdf/version", Value::from(16u32))),
        ("kdf.m_cost_kib", |document| document.set("/kdf/m_cost_kib", Value::from(262_144u32))),
        ("kdf.t_cost", |document| document.set("/kdf/t_cost", Value::from(9u32))),
        ("kdf.p_cost", |document| document.set("/kdf/p_cost", Value::from(2u32))),
        ("kdf.salt", |document| document.flip_hex_byte("/kdf/salt", 0)),
        ("hkdf.hash", |document| document.set("/hkdf/hash", Value::from("sha512"))),
        ("hkdf.salt", |document| document.flip_hex_byte("/hkdf/salt", 0)),
        ("aead.algorithm", |document| document.set("/aead/algorithm", Value::from("aes256gcm"))),
        ("aead.nonce", |document| document.flip_hex_byte("/aead/nonce", 0)),
        ("commitment", |document| document.flip_hex_byte("/commitment", 0)),
    ];

    for (name, mutate) in cases {
        let fixture = filled().unwrap();
        let untouched = fixture.read_file().expect("read");
        let mut forge = fixture.forge().expect("forge");
        assert!(mutate(&mut forge), "{name} is not a field of the file");
        forge.write().expect("write");
        // A mutation that happened to be a no-op would make the case vacuous.
        assert_ne!(fixture.read_file().expect("read"), untouched, "{name} did not change");

        match fixture.unlock() {
            Err(VaultError::WrongPassword) => {
                panic!("{name}: tampering must never look like a wrong password")
            }
            Err(_) => {}
            Ok(_) => panic!("{name}: a tampered file must not open"),
        }
    }
}

#[test]
fn flipping_a_ciphertext_byte_is_corruption_and_not_a_wrong_password() {
    let length =
        hex::decode(filled().unwrap().forge().expect("forge").string("/ciphertext").expect("hex"))
            .expect("hex")
            .len();
    let middle = length.checked_div(2).unwrap_or_default();
    for index in [0usize, 1, middle, length.saturating_sub(1)] {
        let fixture = filled().unwrap();
        let mut forge = fixture.forge().expect("forge");
        assert!(forge.flip_hex_byte("/ciphertext", index));
        forge.write().expect("write");
        match fixture.unlock() {
            Err(VaultError::Corrupt { .. }) => {}
            other => panic!("byte {index}: expected corruption, got {other:?}"),
        }
    }
}

#[test]
fn an_attacker_holding_the_master_key_still_needs_the_password() {
    let fixture = filled().unwrap();
    let master = fixture.master_key().expect("master key");

    let donor = TempVault::new("other-wallet").expect("temp dir");
    let mut opened = donor.vault().create(&password("a different password")).expect("create");
    opened.insert(blob(BlobContent::SeedEntropy, 0x99, 32)).expect("insert");
    opened.lock();
    let donated = donor.forge().expect("forge");

    let mut forge = fixture.forge().expect("forge");
    for field in ["/aead/nonce", "/ciphertext", "/kdf/salt", "/hkdf/salt", "/commitment"] {
        assert!(forge.set(field, donated.get(field).expect("donor field").clone()));
    }
    forge.restamp(&master).expect("restamp");
    forge.write().expect("write");

    // The header verifies now, so the refusal comes from the password factor alone.
    // Neither password opens it, because the donor's commitment was derived under a
    // different master key and wallet id.
    assert!(matches!(fixture.unlock(), Err(VaultError::WrongPassword)));
    assert!(matches!(
        fixture.vault().unlock(&password("a different password")),
        Err(VaultError::WrongPassword)
    ));
}

#[test]
fn a_transplanted_ciphertext_is_corruption_and_not_a_wrong_password() {
    let fixture = filled().unwrap();
    let master = fixture.master_key().expect("master key");

    let donor = TempVault::new("donor-wallet").expect("temp dir");
    let mut opened = donor.create().expect("create");
    opened.insert(blob(BlobContent::SeedEntropy, 0x99, 32)).expect("insert");
    opened.lock();

    let donated = donor.forge().expect("forge");
    let mut forge = fixture.forge().expect("forge");
    for field in ["/aead/nonce", "/ciphertext"] {
        assert!(forge.set(field, donated.get(field).expect("donor field").clone()));
    }
    forge.restamp(&master).expect("restamp");
    forge.write().expect("write");

    match fixture.unlock() {
        Err(VaultError::Corrupt { .. }) => {}
        other => panic!("expected corruption, got {other:?}"),
    }
}

#[test]
fn a_different_master_key_cannot_verify_the_header() {
    let fixture = filled().unwrap();
    fixture.store().put(WALLET_ID, &MasterKey::from_bytes([0xab; 32])).expect("swap the key");
    assert!(matches!(fixture.unlock(), Err(VaultError::Corrupt { .. })));
}

#[test]
fn a_handle_for_another_wallet_refuses_the_file() {
    let fixture = filled().unwrap();
    assert!(matches!(
        fixture.handle("some-other-wallet", Argon2Params::MINIMUM).unlock(&password(TEST_PASSWORD)),
        Err(VaultError::Corrupt { .. })
    ));
}

#[test]
fn a_truncated_file_is_rejected_before_any_key_is_touched() {
    let fixture = filled().unwrap();
    let bytes = fixture.read_file().expect("read");
    let half = bytes.len().checked_div(2).unwrap_or_default();
    fixture.install(bytes.get(..half).expect("prefix")).expect("write");
    // Any keychain access through this handle fails, so `Malformed` proves there was none.
    let keyless =
        Vault::new(fixture.path(), WALLET_ID, Arc::new(FailingKeyStore), Argon2Params::MINIMUM);
    assert!(matches!(keyless.unlock(&password(TEST_PASSWORD)), Err(VaultError::Malformed(_))));
}

#[test]
fn a_downgraded_kdf_is_refused_before_the_kdf_runs() {
    let fixture = filled().unwrap();
    let master = fixture.master_key().expect("master key");
    let mut forge = fixture.forge().expect("forge");
    assert!(forge.set("/kdf/m_cost_kib", Value::from(8u32)));
    assert!(forge.set("/kdf/t_cost", Value::from(1u32)));
    assert!(forge.set("/kdf/p_cost", Value::from(1u32)));
    forge.restamp(&master).expect("restamp");
    forge.write().expect("write");
    assert!(matches!(fixture.unlock(), Err(VaultError::WeakKdfParams { field: "m_cost_kib" })));
}

#[test]
fn an_unknown_format_version_is_named_rather_than_misparsed() {
    let fixture = filled().unwrap();
    let mut forge = fixture.forge().expect("forge");
    assert!(forge.set("/format_version", Value::from(2u32)));
    forge.write().expect("write");
    assert!(matches!(
        fixture.unlock(),
        Err(VaultError::UnsupportedVersion { found: 2, supported: 3 })
    ));
}

#[test]
fn an_added_or_removed_field_is_not_a_vault() {
    let fixture = filled().unwrap();
    let bytes = fixture.read_file().expect("read");
    let mut json: Value = serde_json::from_slice(&bytes).expect("json");
    json.as_object_mut().expect("object").insert(String::from("extra"), Value::Null);
    fixture.install(&serde_json::to_vec(&json).expect("encode")).expect("write");
    assert!(matches!(fixture.unlock(), Err(VaultError::Malformed(_))));

    let mut json: Value = serde_json::from_slice(&bytes).expect("json");
    json.as_object_mut().expect("object").remove("commitment");
    fixture.install(&serde_json::to_vec(&json).expect("encode")).expect("write");
    assert!(matches!(fixture.unlock(), Err(VaultError::Malformed(_))));
}
