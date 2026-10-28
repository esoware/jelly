use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;

use jelly_core::KeyKind;
use jelly_vault::{
    Argon2Params, BlobContent, BlobKind, EntryId, MasterKey, MasterKeyStore, Vault, VaultError,
    test_util::{
        FailingKeyStore, MemoryKeyStore, TEST_PASSWORD, TempVault, missing_entry_id, password,
    },
};

use crate::common::{WALLET_ID, blob, ids_of_kind, key, seed};

#[test]
fn the_full_entry_lifecycle_survives_a_lock_and_unlock() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    assert_eq!(vault.entry_count(), 0);
    assert_eq!(vault.kdf_params(), Argon2Params::MINIMUM);

    let seed_id = vault.insert(seed()).expect("insert seed");
    let key_id = vault.insert(key()).expect("insert key");
    let scan_id =
        vault.insert(blob(BlobContent::MoneroScanState, 0x33, 8)).expect("insert scan state");
    assert_eq!(vault.entry_count(), 3);
    assert_eq!(ids_of_kind(&vault, BlobKind::SeedEntropy), vec![seed_id]);
    assert!(ids_of_kind(&vault, BlobKind::SeedPassphrase).is_empty());

    vault
        .replace(scan_id, blob(BlobContent::MoneroScanState, 0x44, 9))
        .expect("replace scan state");
    vault.remove(key_id).expect("remove key");
    assert!(matches!(vault.read(key_id), Err(VaultError::EntryNotFound(_))));
    vault.lock();

    let reopened = fixture.unlock().expect("unlock");
    assert_eq!(reopened.entry_count(), 2);
    assert_eq!(ids_of_kind(&reopened, BlobKind::SeedEntropy), vec![seed_id]);
    assert_eq!(ids_of_kind(&reopened, BlobKind::MoneroScanState), vec![scan_id]);
    assert!(ids_of_kind(&reopened, BlobKind::PrivateKey).is_empty());
    reopened.read(seed_id).expect("read seed").with_bytes(|bytes| assert_eq!(bytes, &[0x11; 32]));
    reopened.read(scan_id).expect("read scan").with_bytes(|bytes| assert_eq!(bytes, &[0x44; 9]));
    assert!(matches!(reopened.read(key_id), Err(VaultError::EntryNotFound(_))));
}

#[test]
fn every_blob_content_survives_a_reopen() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let entries: Vec<(EntryId, BlobContent)> = [
        BlobContent::SeedEntropy,
        BlobContent::SeedPassphrase,
        BlobContent::PassphraseVerifier,
        BlobContent::MoneroScanState,
    ]
    .into_iter()
    .chain(KeyKind::ALL.map(BlobContent::PrivateKey))
    .map(|content| (vault.insert(blob(content, 0x77, 32)).expect("insert"), content))
    .collect();
    vault.lock();

    let reopened = fixture.unlock().expect("unlock");
    for (id, content) in entries {
        assert_eq!(reopened.read(id).expect("read").content(), content);
    }
}

#[test]
fn only_a_private_key_entry_yields_key_material() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let seed_id = vault.insert(seed()).expect("insert seed");
    let key_id = vault.insert(key()).expect("insert key");

    let material = vault.key_material(key_id).expect("key material");
    assert_eq!(material.kind(), KeyKind::Secp256k1);
    material.with_bytes(|bytes| assert_eq!(bytes, &[0x22; 32]));
    assert!(matches!(vault.key_material(seed_id), Err(VaultError::NotKeyMaterial(_))));
}

#[test]
fn an_id_the_vault_never_issued_is_not_found() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    vault.insert(seed()).expect("insert seed");
    vault.insert(key()).expect("insert key");

    assert!(matches!(vault.key_material(missing_entry_id()), Err(VaultError::EntryNotFound(_))));
    assert!(matches!(vault.replace(missing_entry_id(), seed()), Err(VaultError::EntryNotFound(_))));
    assert!(matches!(vault.remove(missing_entry_id()), Err(VaultError::EntryNotFound(_))));
}

#[test]
fn read_first_finds_the_single_entry_of_a_kind() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    assert!(vault.read_first(BlobKind::SeedEntropy).is_none());
    vault.insert(seed()).expect("insert");
    let found = vault.read_first(BlobKind::SeedEntropy).expect("present");
    found.with_bytes(|bytes| assert_eq!(bytes, &[0x11; 32]));
}

#[test]
fn creating_over_an_existing_vault_is_refused() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    fixture.create().expect("create").lock();
    assert!(matches!(fixture.create(), Err(VaultError::AlreadyExists(_))));
}

#[test]
fn concurrent_creations_preserve_the_winning_vault_and_master_key() {
    for is_same_path in [true, false] {
        let fixture = TempVault::new(WALLET_ID).unwrap();
        let second_path = if is_same_path {
            fixture.path().to_path_buf()
        } else {
            fixture.path().with_file_name("second.vault")
        };
        let second = Vault::new(
            &second_path,
            WALLET_ID,
            Arc::<MemoryKeyStore>::clone(fixture.store()),
            Argon2Params::MINIMUM,
        );
        let ready = Barrier::new(3);
        let outcomes = thread::scope(|scope| {
            let first_creation = scope.spawn(|| {
                ready.wait();
                fixture.vault().create(&password("first password"))
            });
            let second_creation = scope.spawn(|| {
                ready.wait();
                second.create(&password("second password"))
            });
            ready.wait();
            (first_creation.join().unwrap(), second_creation.join().unwrap())
        });
        match outcomes {
            (Ok(winner), Err(VaultError::AlreadyExists(_) | VaultError::KeychainOccupied)) => {
                winner.lock();
                fixture.vault().unlock(&password("first password")).unwrap().lock();
                if !is_same_path {
                    assert!(!second_path.exists());
                }
            }
            (Err(VaultError::AlreadyExists(_) | VaultError::KeychainOccupied), Ok(winner)) => {
                winner.lock();
                second.unlock(&password("second password")).unwrap().lock();
                if !is_same_path {
                    assert!(!fixture.path().exists());
                }
            }
            outcomes => panic!("exactly one creation must win: {outcomes:?}"),
        }
        assert!(fixture.store().contains(WALLET_ID));
    }
}

#[derive(Debug)]
struct AppearingFileStore {
    store: MemoryKeyStore,
    path: PathBuf,
}

impl MasterKeyStore for AppearingFileStore {
    fn get(&self, wallet_id: &str) -> Result<Option<MasterKey>, VaultError> {
        self.store.get(wallet_id)
    }

    fn put(&self, wallet_id: &str, key: &MasterKey) -> Result<(), VaultError> {
        fs::write(&self.path, b"an independently appearing file")
            .map_err(|source| VaultError::Io { path: self.path.clone(), source })?;
        self.store.put(wallet_id, key)
    }

    fn delete(&self, wallet_id: &str) -> Result<(), VaultError> {
        self.store.delete(wallet_id)
    }
}

#[test]
fn a_file_that_appears_mid_create_is_kept_and_the_new_master_key_removed() {
    let fixture = TempVault::new(WALLET_ID).unwrap();
    let store = Arc::new(AppearingFileStore {
        store: MemoryKeyStore::new(),
        path: fixture.path().to_path_buf(),
    });
    let vault = Vault::new(
        fixture.path(),
        WALLET_ID,
        Arc::<AppearingFileStore>::clone(&store),
        Argon2Params::MINIMUM,
    );
    assert!(matches!(vault.create(&password(TEST_PASSWORD)), Err(VaultError::AlreadyExists(_))));
    assert_eq!(fs::read(fixture.path()).unwrap(), b"an independently appearing file");
    assert!(!store.store.contains(WALLET_ID));
}

#[test]
fn unlock_tells_a_missing_file_from_a_missing_master_key() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    assert!(!fixture.path().exists());
    assert!(matches!(fixture.unlock(), Err(VaultError::FileNotFound(_))));

    fixture.create().expect("create").lock();
    assert!(fixture.path().exists());
    fixture.unlock().expect("both factors are present").lock();

    fixture.store().delete(WALLET_ID).expect("delete the master key");
    assert!(fixture.path().exists(), "the file is still there");
    assert!(matches!(fixture.unlock(), Err(VaultError::KeychainMissing)));
}

#[test]
fn a_wrong_password_is_reported_as_such_and_costs_nothing_else() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    fixture.create().expect("create").lock();
    let before = fixture.read_file().expect("read");

    for _ in 0..4 {
        assert!(matches!(
            fixture.vault().unlock(&password("something else entirely")),
            Err(VaultError::WrongPassword)
        ));
    }
    assert_eq!(fixture.read_file().expect("read"), before, "a failed unlock writes nothing");
    fixture.unlock().expect("unlock right away").lock();
}

#[test]
fn a_keychain_failure_is_not_a_missing_master_key() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let vault =
        Vault::new(fixture.path(), WALLET_ID, Arc::new(FailingKeyStore), Argon2Params::MINIMUM);
    assert!(matches!(vault.is_claimed(), Err(VaultError::Keychain(_))));
    assert!(matches!(vault.create(&password(TEST_PASSWORD)), Err(VaultError::Keychain(_))));
    assert!(!fixture.path().exists(), "a failed create must not leave a file behind");

    fixture.create().expect("create").lock();
    assert!(matches!(vault.unlock(&password(TEST_PASSWORD)), Err(VaultError::Keychain(_))));
}

#[test]
fn creating_a_vault_never_overwrites_an_existing_master_key() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let seed_id = vault.insert(seed()).expect("insert");
    vault.lock();

    let moved = fixture.path().with_file_name("moved-aside.vault");
    fs::rename(fixture.path(), &moved).expect("move the vault aside");
    let second = fixture.vault().create(&password("a second wallet"));
    assert!(matches!(second, Err(VaultError::KeychainOccupied)), "{second:?}");

    let original = Vault::new(
        moved,
        WALLET_ID,
        Arc::<MemoryKeyStore>::clone(fixture.store()),
        Argon2Params::MINIMUM,
    );
    original
        .unlock(&password(TEST_PASSWORD))
        .expect("the moved vault lost its master key to a later create")
        .read(seed_id)
        .expect("read")
        .with_bytes(|bytes| assert_eq!(bytes, &[0x11; 32]));
}

/// A malformed credential fails to read, yet the platform store still accepts a write
/// over it.
#[derive(Debug)]
struct UnreadableKeyStore(MemoryKeyStore);

impl MasterKeyStore for UnreadableKeyStore {
    fn get(&self, _wallet_id: &str) -> Result<Option<MasterKey>, VaultError> {
        Err(VaultError::KeychainMalformed)
    }

    fn put(&self, wallet_id: &str, key: &MasterKey) -> Result<(), VaultError> {
        self.0.put(wallet_id, key)
    }

    fn delete(&self, wallet_id: &str) -> Result<(), VaultError> {
        self.0.delete(wallet_id)
    }
}

#[test]
fn creating_a_vault_never_overwrites_a_master_key_it_cannot_read() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let store = Arc::new(UnreadableKeyStore(MemoryKeyStore::with_key(WALLET_ID, [0x5a; 32])));
    let vault = Vault::new(
        fixture.path(),
        WALLET_ID,
        Arc::<UnreadableKeyStore>::clone(&store),
        Argon2Params::MINIMUM,
    );

    let created = vault.create(&password(TEST_PASSWORD));
    assert!(matches!(created, Err(VaultError::KeychainMalformed)), "{created:?}");
    assert_eq!(store.0.key_bytes(WALLET_ID), Some([0x5a; 32]), "the stored key was replaced");
    assert!(!fixture.path().exists(), "a refused create must not leave a file behind");
}

#[test]
fn changing_the_password_rewrites_every_derived_value() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let seed_id = vault.insert(seed()).expect("insert");
    let before = fixture.forge().expect("forge");

    let new_password = password("a completely different phrase");
    assert!(matches!(
        vault.change_password(&password("not the current one"), &new_password),
        Err(VaultError::WrongPassword)
    ));
    vault.change_password(&password(TEST_PASSWORD), &new_password).expect("change password");
    vault.lock();

    let after = fixture.forge().expect("forge");
    for field in
        ["/kdf/salt", "/hkdf/salt", "/aead/nonce", "/commitment", "/ciphertext", "/header_tag"]
    {
        assert_ne!(before.string(field), after.string(field), "{field} must be fresh");
    }

    assert!(matches!(fixture.unlock(), Err(VaultError::WrongPassword)));
    let reopened = fixture.vault().unlock(&new_password).expect("unlock under the new password");
    reopened.read(seed_id).expect("read").with_bytes(|bytes| assert_eq!(bytes, &[0x11; 32]));
}

#[test]
fn any_password_creates_and_rotates() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    fixture.vault().create(&password("J")).expect("create with a 1-char password").lock();
    assert!(fixture.path().exists());

    let mut vault = fixture.vault().unlock(&password("J")).expect("unlock");
    vault.change_password(&password("J"), &password("")).expect("rotate to an empty password");
    vault.lock();
    fixture.vault().unlock(&password("")).expect("unlock under the empty password").lock();
}

#[test]
fn an_export_checks_the_password_before_it_looks_up_the_entry() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    assert!(matches!(
        vault.export(&password(TEST_PASSWORD), BlobKind::SeedEntropy),
        Err(VaultError::NoEntryOfKind(BlobKind::SeedEntropy))
    ));
    vault.insert(seed()).expect("insert");

    vault.verify_password(&password(TEST_PASSWORD)).expect("the session password verifies");
    assert!(matches!(
        vault.verify_password(&password("not the password")),
        Err(VaultError::WrongPassword)
    ));
    // The password is checked before the lookup, so a wrong one can't probe what the
    // vault holds.
    assert!(matches!(
        vault.export(&password("not the password"), BlobKind::SeedPassphrase),
        Err(VaultError::WrongPassword)
    ));
    vault
        .export(&password(TEST_PASSWORD), BlobKind::SeedEntropy)
        .expect("export")
        .with_bytes(|bytes| assert_eq!(bytes, &[0x11; 32]));
}

#[test]
fn destroying_the_vault_removes_the_file_and_the_master_key() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    fixture.create().expect("create").lock();
    assert!(fixture.store().contains(WALLET_ID));

    fixture.vault().destroy().expect("destroy");
    assert!(!fixture.path().exists());
    assert!(!fixture.store().contains(WALLET_ID));
    fixture.vault().destroy().expect("destroy again");
}

#[test]
fn a_weaker_file_is_upgraded_on_unlock_and_a_stronger_one_is_left_alone() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let seed_id = vault.insert(seed()).expect("insert");
    vault.lock();
    let floor = Argon2Params::MINIMUM;
    assert_eq!(
        fixture.forge().expect("forge").get("/kdf/t_cost"),
        Some(&serde_json::Value::from(floor.t_cost()))
    );

    let stronger = Argon2Params::new(floor.m_cost_kib(), floor.t_cost().saturating_add(1), 1);
    let handle = fixture.handle(WALLET_ID, stronger);
    let upgraded = handle.unlock(&password(TEST_PASSWORD)).expect("unlock");
    assert_eq!(upgraded.kdf_params(), stronger);
    upgraded.read(seed_id).expect("the payload survived the rekey");
    upgraded.lock();

    let opened = fixture.unlock().expect("unlock under the weaker handle");
    assert_eq!(opened.kdf_params(), stronger, "an unlock must never downgrade a file");
}

#[test]
fn an_upgrade_never_lowers_an_axis_the_file_is_stronger_on() {
    let floor = Argon2Params::MINIMUM;
    let file = Argon2Params::new(floor.m_cost_kib().saturating_mul(2), 2, 1);
    let handle = Argon2Params::new(floor.m_cost_kib(), 3, 1);

    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault =
        fixture.handle(WALLET_ID, file).create(&password(TEST_PASSWORD)).expect("create");
    let seed_id = vault.insert(seed()).expect("insert");
    vault.lock();

    let opened =
        fixture.handle(WALLET_ID, handle).unlock(&password(TEST_PASSWORD)).expect("unlock");
    assert_eq!(opened.kdf_params(), Argon2Params::new(file.m_cost_kib(), 3, 1));
    opened.read(seed_id).expect("the payload survived the rekey");
    opened.lock();

    let reopened = fixture.unlock().expect("reopen");
    assert_eq!(reopened.kdf_params(), Argon2Params::new(file.m_cost_kib(), 3, 1));
}

#[test]
fn changing_the_password_never_weakens_the_file() {
    let strong = Argon2Params::new(Argon2Params::MINIMUM.m_cost_kib().saturating_mul(2), 3, 1);
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    fixture.handle(WALLET_ID, strong).create(&password(TEST_PASSWORD)).expect("create").lock();

    // The fixture's own handle is at the floor, which is weaker on memory and passes.
    let mut vault = fixture.unlock().expect("unlock");
    assert_eq!(vault.kdf_params(), strong);
    vault.change_password(&password(TEST_PASSWORD), &password("a new one")).expect("rotate");
    assert_eq!(vault.kdf_params(), strong);
    vault.lock();

    let reopened = fixture.vault().unlock(&password("a new one")).expect("reopen");
    assert_eq!(reopened.kdf_params(), strong);
}

#[test]
fn a_large_vault_round_trips() {
    let fixture = TempVault::new(WALLET_ID).expect("temp dir");
    let mut vault = fixture.create().expect("create");
    let mut ids = Vec::with_capacity(200);
    for i in 0..200u16 {
        let byte = u8::try_from(i % 251).expect("fits a byte");
        ids.push(
            vault
                .insert(blob(BlobContent::PrivateKey(KeyKind::Ed25519Seed), byte, 512))
                .expect("insert"),
        );
    }
    vault.lock();

    let reopened = fixture.unlock().expect("unlock");
    assert_eq!(reopened.entry_count(), 200);
    assert!(fixture.read_file().expect("read").len() > 200 * 512);
    for (i, id) in ids.into_iter().enumerate() {
        let byte = u8::try_from(i % 251).expect("fits a byte");
        reopened.read(id).expect("read").with_bytes(|bytes| assert_eq!(bytes, &vec![byte; 512]));
    }
}
