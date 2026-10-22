use std::error::Error;
use std::fmt::Write as _;
use std::sync::Arc;

use jelly_core::KeyKind;
use jelly_vault::{
    Argon2Params, BlobContent, BlobKind, MasterKey, MasterKeyStore as _, SecretBlob, Vault,
    VaultError, VaultPassword,
    test_util::{
        FailingKeyStore, MemoryKeyStore, TEST_PASSWORD, TempVault, missing_entry_id, password,
    },
};
use secrecy::SecretString;
use zeroize::Zeroizing;

use crate::common::WALLET_ID;

/// Distinctive enough that a substring search is meaningful.
const SECRET: &[u8] = b"peculiar-vault-secret-material-0123456789";
const PASSWORD: &str = "distinctive-password-not-in-any-message";

fn contains_a_secret(rendered: &str) -> bool {
    let haystack = rendered.to_lowercase();
    let plain = String::from_utf8_lossy(SECRET).to_lowercase();
    haystack.contains(&hex::encode(SECRET))
        || haystack.contains(plain.as_str())
        || haystack.contains(PASSWORD)
        || haystack.contains(TEST_PASSWORD)
}

fn assert_no_secret(label: &str, rendered: &str) {
    assert!(!contains_a_secret(rendered), "{label} leaked: {rendered}");
}

#[test]
fn every_secret_bearing_type_redacts() {
    assert_no_secret(
        "VaultPassword",
        &format!("{:?}", VaultPassword::new(SecretString::from(PASSWORD))),
    );
    let master = format!("{:?}", MasterKey::from_bytes([0xab; 32]));
    assert_no_secret("MasterKey", &master);
    assert!(!master.contains(&hex::encode([0xab; 32])));
    let blob = SecretBlob::new(
        BlobContent::PrivateKey(KeyKind::Secp256k1),
        Zeroizing::new(SECRET.to_vec()),
    );
    assert_no_secret("SecretBlob", &format!("{blob:?}"));
    assert!(format!("{blob:?}").contains("<redacted"));

    let store = MemoryKeyStore::with_key(WALLET_ID, [0xcd; 32]);
    assert_no_secret("MemoryKeyStore", &format!("{store:?}"));
    assert!(!format!("{store:?}").contains(&hex::encode([0xcd; 32])));
}

#[test]
fn an_open_vault_and_its_handle_redact() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.vault().create(&password(PASSWORD)).expect("create");
    let id = vault
        .insert(SecretBlob::new(BlobContent::SeedEntropy, Zeroizing::new(SECRET.to_vec())))
        .expect("insert");
    assert_no_secret("UnlockedVault", &format!("{vault:?}"));
    assert_no_secret("Vault", &format!("{:?}", fixture.vault()));
    assert_no_secret("EntryId", &format!("{id:?}"));
    assert_no_secret("read", &format!("{:?}", vault.read(id).expect("read")));
    assert_no_secret("file", &String::from_utf8_lossy(&fixture.read_file().expect("read")));
}

#[test]
fn no_error_carries_a_fragment_of_its_input() {
    let rendered = |error: VaultError| {
        let mut text = format!("{error} | {error:?}");
        let mut source: Option<&(dyn Error + 'static)> = error.source();
        while let Some(cause) = source {
            let _ = write!(text, " | {cause}");
            source = cause.source();
        }
        text
    };

    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let missing = fixture.vault().unlock(&password(PASSWORD)).expect_err("no file");
    assert_no_secret("unlock without a file", &rendered(missing));

    let mut vault = fixture.vault().create(&password(PASSWORD)).expect("create");
    let id = vault
        .insert(SecretBlob::new(BlobContent::SeedEntropy, Zeroizing::new(SECRET.to_vec())))
        .expect("insert");

    let cases: Vec<(&str, VaultError)> = vec![
        ("create over an existing file", fixture.vault().create(&password(PASSWORD)).err()),
        ("wrong password", fixture.vault().unlock(&password("nope")).err()),
        ("verify a wrong password", vault.verify_password(&password("nope")).err()),
        (
            "export under a wrong password",
            vault.export(&password("nope"), BlobKind::SeedEntropy).err(),
        ),
        ("read a missing entry", vault.read(missing_entry_id()).err()),
        ("key material from a seed", vault.key_material(id).err()),
        ("replace a missing entry", {
            let blob = SecretBlob::new(BlobContent::SeedEntropy, Zeroizing::new(SECRET.to_vec()));
            vault.replace(missing_entry_id(), blob).err()
        }),
        ("remove a missing entry", vault.remove(missing_entry_id()).err()),
    ]
    .into_iter()
    .map(|(label, error)| (label, error.unwrap_or_else(|| panic!("{label} did not fail"))))
    .collect();
    for (label, error) in cases {
        assert_no_secret(label, &rendered(error));
    }

    let bytes = fixture.read_file().expect("read");
    let half = bytes.len().checked_div(2).unwrap_or_default();
    fixture.install(bytes.get(..half).expect("prefix")).expect("write");
    assert_no_secret(
        "malformed file",
        &rendered(fixture.vault().unlock(&password(PASSWORD)).expect_err("malformed")),
    );

    fixture.install(&bytes).expect("restore");
    fixture.store().put(WALLET_ID, &MasterKey::from_bytes([0u8; 32])).expect("swap");
    assert_no_secret(
        "corrupt file",
        &rendered(fixture.vault().unlock(&password(PASSWORD)).expect_err("corrupt")),
    );

    let broken =
        Vault::new(fixture.path(), WALLET_ID, Arc::new(FailingKeyStore), Argon2Params::MINIMUM);
    assert_no_secret(
        "broken keychain",
        &rendered(broken.unlock(&password(PASSWORD)).expect_err("keychain")),
    );
}
