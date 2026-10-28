//! A committed vault file that this build must still open.
//!
//! Every other test in this crate writes and reads with the same code, so none of them
//! can catch format drift. This file only opens if the Argon2id parameter encoding, the
//! HKDF salt, keying material, and info strings, the key commitment, the AEAD associated
//! data, the canonical header MAC message, and the payload encoding are all unchanged
//! since it was recorded. Don't regenerate it to make drift go away.
//!
//! It's written at [`Argon2Params::MINIMUM`] to keep the suite fast. The parameters live
//! in the header, so any value runs the same code, and `src/kdf.rs` pins the shipped
//! baseline.
//!
//! After a deliberate format change, record a new fixture with this command.
//!
//! ```text
//! cargo test -p jelly_vault --test it regenerate_golden -- --ignored
//! ```

use std::error::Error;
use std::fs;
use std::path::PathBuf;

use jelly_core::KeyKind;
use jelly_vault::{
    Argon2Params, BlobContent, BlobKind, MasterKey, MasterKeyStore as _, VaultError,
    test_util::{TempVault, password},
};
use serde_json::Value;

use crate::common::{blob, ids_of_kind};

/// The committed file's wallet id and password. Its master key is recorded beside it,
/// because [`jelly_vault::Vault::create`] draws its own. Test material that protects
/// nothing.
const WALLET_ID: &str = "jelly-golden-wallet";
const PASSWORD: &str = "correct horse battery staple";

/// What the fixture holds. Ids are random at write time, so entries are looked up by kind.
const CONTENTS: [(BlobKind, BlobContent, u8, usize); 5] = [
    (BlobKind::SeedEntropy, BlobContent::SeedEntropy, 0x11, 32),
    (BlobKind::PrivateKey, BlobContent::PrivateKey(KeyKind::Secp256k1), 0x22, 32),
    (BlobKind::MoneroScanState, BlobContent::MoneroScanState, 0x33, 8),
    (BlobKind::SeedPassphrase, BlobContent::SeedPassphrase, 0x44, 7),
    (BlobKind::PassphraseVerifier, BlobContent::PassphraseVerifier, 0x55, 32),
];

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden").join(name)
}

fn read_golden(name: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let path = golden(name);
    Ok(fs::read(&path).map_err(|error| {
        format!("read {}: {error}; record one with regenerate_golden", path.display())
    })?)
}

fn master_key() -> Result<[u8; 32], Box<dyn Error>> {
    let text = String::from_utf8(read_golden("v3.master-key")?)?;
    Ok(<[u8; 32]>::try_from(hex::decode(text.trim())?.as_slice())?)
}

/// Stages the fixture in a temp directory so no test writes to the committed file.
fn staged(wallet_id: &str) -> Result<TempVault, Box<dyn Error>> {
    let fixture = TempVault::with_master_key(wallet_id, master_key()?)?;
    fixture.install(&read_golden("v3.vault")?)?;
    Ok(fixture)
}

#[test]
#[ignore = "records a new fixture; run with --ignored after a deliberate format change"]
fn regenerate_golden() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.vault().create(&password(PASSWORD)).expect("create");
    for (_, content, byte, length) in CONTENTS {
        vault.insert(blob(content, byte, length)).expect("insert");
    }
    vault.lock();

    let key = fixture.store().key_bytes(WALLET_ID).expect("the store holds the master key");
    fs::create_dir_all(golden("v3.vault").parent().expect("parent"))
        .expect("create the golden directory");
    fs::write(golden("v3.vault"), fixture.read_file().expect("read")).expect("write the fixture");
    fs::write(golden("v3.master-key"), hex::encode(key)).expect("write the master key");
}

#[test]
fn the_committed_header_has_the_v3_shape() {
    let json: Value = serde_json::from_slice(&read_golden("v3.vault").unwrap()).expect("json");
    let get = |pointer: &str| json.pointer(pointer).unwrap_or_else(|| panic!("{pointer}"));
    let text = |pointer: &str| get(pointer).as_str().unwrap_or_else(|| panic!("{pointer}"));

    assert_eq!(text("/magic"), "jelly-vault");
    assert_eq!(get("/format_version"), &Value::from(3u32));
    assert_eq!(text("/wallet_id"), WALLET_ID);
    assert_eq!(text("/kdf/algorithm"), "argon2id");
    assert_eq!(get("/kdf/version"), &Value::from(19u32));
    assert_eq!(get("/kdf/m_cost_kib"), &Value::from(Argon2Params::MINIMUM.m_cost_kib()));
    assert_eq!(get("/kdf/t_cost"), &Value::from(Argon2Params::MINIMUM.t_cost()));
    assert_eq!(get("/kdf/p_cost"), &Value::from(Argon2Params::MINIMUM.p_cost()));
    assert_eq!(text("/kdf/salt").len(), 32);
    assert_eq!(text("/hkdf/hash"), "sha256");
    assert_eq!(text("/hkdf/salt").len(), 64);
    assert_eq!(text("/aead/algorithm"), "xchacha20poly1305");
    assert_eq!(text("/aead/nonce").len(), 48);
    assert_eq!(text("/commitment").len(), 64);
    assert_eq!(text("/header_tag").len(), 64);
    assert_eq!(json.as_object().expect("object").len(), 9, "no field was added or dropped");
}

#[test]
fn the_committed_vault_still_opens_and_holds_what_it_held() {
    let fixture = staged(WALLET_ID).unwrap();
    let unlocked =
        fixture.vault().unlock(&password(PASSWORD)).expect("the golden vault still opens");

    assert_eq!(unlocked.entry_count(), CONTENTS.len());
    assert_eq!(unlocked.kdf_params(), Argon2Params::MINIMUM);
    for (kind, content, byte, length) in CONTENTS {
        let blob = unlocked.read_first(kind).unwrap_or_else(|| panic!("no {kind} entry"));
        assert_eq!(blob.content(), content);
        blob.with_bytes(|bytes| assert_eq!(bytes, vec![byte; length]));
    }

    let key_id = *ids_of_kind(&unlocked, BlobKind::PrivateKey).first().expect("private key");
    let material = unlocked.key_material(key_id).expect("key material");
    assert_eq!(material.kind(), KeyKind::Secp256k1);
    material.with_bytes(|bytes| assert_eq!(bytes, vec![0x22; 32]));
}

#[test]
fn the_committed_vault_needs_both_of_its_factors() {
    let fixture = staged(WALLET_ID).unwrap();
    assert!(matches!(
        fixture.vault().unlock(&password("not the password")),
        Err(VaultError::WrongPassword)
    ));

    let fixture = staged(WALLET_ID).unwrap();
    fixture.store().put(WALLET_ID, &MasterKey::from_bytes([0u8; 32])).expect("swap the master key");
    assert!(matches!(fixture.vault().unlock(&password(PASSWORD)), Err(VaultError::Corrupt { .. })));

    let fixture = staged("another-wallet").unwrap();
    assert!(matches!(fixture.vault().unlock(&password(PASSWORD)), Err(VaultError::Corrupt { .. })));
}

/// Rewriting the fixture under the same factors gives a different file that still opens,
/// so today's writer and the recorded format agree.
#[test]
fn the_committed_vault_can_be_rewritten_and_reopened() {
    let fixture = staged(WALLET_ID).unwrap();
    let before = fixture.read_file().expect("read");

    let mut unlocked = fixture.vault().unlock(&password(PASSWORD)).expect("unlock");
    let added = unlocked
        .insert(blob(BlobContent::PrivateKey(KeyKind::MoneroSpend), 0x66, 32))
        .expect("insert");
    unlocked.lock();
    assert_ne!(fixture.read_file().expect("read"), before);

    let reopened = fixture.vault().unlock(&password(PASSWORD)).expect("reopen");
    assert_eq!(reopened.entry_count(), CONTENTS.len().saturating_add(1));
    reopened.read(added).expect("read").with_bytes(|bytes| assert_eq!(bytes, vec![0x66; 32]));
}
