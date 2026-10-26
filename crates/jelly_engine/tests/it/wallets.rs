use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use jelly_core::{ChainId, CoreError, PhraseWords, SecretText, WalletColor};
use jelly_engine::{
    EngineDeps, LockReason, LockState, PassphraseInput, StartupError, WalletEngine, WalletRecord,
    test_util,
};
use jelly_vault::{
    Argon2Params, MasterKey, MasterKeyStore, Vault, VaultError,
    test_util::{MemoryKeyStore, password},
};

use crate::common::{OTHER_PASSWORD, PASSWORD, harness, harness_in, harness_with_keystore, secret};

#[tokio::test]
async fn a_new_seed_wallet_opens_and_reopens() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("first").await.unwrap();
    assert_eq!(created.phrase.split_whitespace().count(), 12);
    assert_eq!(created.wallet.color, WalletColor::default());
    assert!(harness.engine.lock_state_now().is_unlocked());

    assert_eq!(harness.engine.list_wallets().await.unwrap().len(), 1);

    harness.engine.lock_wallet().await;
    assert_eq!(harness.engine.lock_state_now(), LockState::locked(LockReason::Requested));

    let reopened =
        harness.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();
    assert_eq!(reopened.id, created.wallet.id);
    assert_eq!(
        harness.engine.lock_state_now(),
        LockState::Unlocked { wallet_id: created.wallet.id, wallet_name: "first".to_owned() }
    );
}

#[tokio::test]
async fn wallet_colors_survive_every_creation_flow_an_edit_and_restart() {
    let harness = harness().await.unwrap();
    let seed_color = WalletColor::parse("#a855f7").unwrap();
    let restored_color = WalletColor::parse("#ff0000").unwrap();
    let edited_color = WalletColor::parse("#22c55e").unwrap();
    let seed = harness
        .engine
        .create_seed_wallet(
            "seed",
            seed_color.clone(),
            secret(PASSWORD),
            PhraseWords::Twelve,
            PassphraseInput::None,
        )
        .await
        .unwrap();
    let restored = harness
        .engine
        .import_seed_wallet(
            "restored",
            restored_color.clone(),
            secret(PASSWORD),
            SecretText::new(seed.phrase.to_string()),
            PassphraseInput::None,
        )
        .await
        .unwrap();
    assert_eq!(seed.wallet.color, seed_color);
    assert_eq!(restored.color, restored_color);
    harness.engine.update_wallet(restored.id, "edited", edited_color.clone()).await.unwrap();
    let edited =
        WalletRecord { name: "edited".to_owned(), color: edited_color.clone(), ..restored };

    let keystore = Arc::clone(&harness.keystore);
    let scratch = harness.shut_down().await.unwrap();
    let harness = harness_in(test_util::config(), scratch, keystore).await.unwrap();
    assert_eq!(harness.engine.list_wallets().await.unwrap(), vec![seed.wallet, edited.clone()]);
    let reopened = harness.engine.unlock_wallet(edited.id, secret(PASSWORD), None).await.unwrap();
    assert_eq!(reopened.color, edited_color);
}

#[tokio::test]
async fn debug_of_a_created_wallet_leaves_out_its_phrase() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("printed").await.unwrap();
    let rendered = format!("{created:?}");
    let words: Vec<&str> = created.phrase.split_whitespace().collect();
    assert!(!rendered.contains(&words.join(" ")), "the seed phrase reached a debug rendering");
}

#[tokio::test]
async fn a_wrong_password_does_not_open_a_wallet() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("first").await.unwrap();
    harness.engine.lock_wallet().await;

    let refused =
        harness.engine.unlock_wallet(created.wallet.id, secret(OTHER_PASSWORD), None).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    assert!(!harness.engine.lock_state_now().is_unlocked());
}

#[tokio::test]
async fn a_one_character_password_is_accepted() {
    let harness = harness().await.unwrap();
    let created = harness
        .engine
        .create_seed_wallet(
            "tiny",
            WalletColor::default(),
            secret("J"),
            PhraseWords::Twelve,
            PassphraseInput::None,
        )
        .await
        .unwrap();
    harness.engine.lock_wallet().await;
    harness.engine.unlock_wallet(created.wallet.id, secret("J"), None).await.unwrap();
    assert!(harness.engine.lock_state_now().is_unlocked());
}

#[tokio::test]
async fn an_imported_phrase_reproduces_the_same_wallet() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("source").await.unwrap();
    let phrase = created.phrase.to_string();
    let first = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    harness.engine.lock_wallet().await;

    let imported = harness
        .engine
        .import_seed_wallet(
            "copy",
            WalletColor::default(),
            secret(PASSWORD),
            SecretText::new(phrase),
            PassphraseInput::None,
        )
        .await
        .unwrap();
    let second = harness.engine.add_address(ChainId::Bitcoin, None, None).await.unwrap();
    assert_ne!(imported.id, created.wallet.id);
    assert_eq!(first.address, second.address);
}

#[tokio::test]
async fn editing_keeps_the_session_and_the_row_in_step() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("before").await.unwrap();
    harness.engine.update_wallet(created.wallet.id, "after", created.wallet.color).await.unwrap();
    assert_eq!(
        harness.engine.lock_state_now(),
        LockState::Unlocked { wallet_id: created.wallet.id, wallet_name: "after".to_owned() }
    );
}

#[tokio::test]
async fn editing_only_the_color_announces_no_lock_change() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("open").await.unwrap();
    let states = harness.engine.lock_state();
    let color = WalletColor::parse("#22c55e").unwrap();
    harness.engine.update_wallet(created.wallet.id, "open", color).await.unwrap();
    assert!(!states.has_changed().unwrap());
}

#[tokio::test]
async fn editing_another_wallet_leaves_the_open_one_alone() {
    let harness = harness().await.unwrap();
    let other = harness.seed_wallet("other").await.unwrap();
    harness.engine.lock_wallet().await;
    let open = harness.seed_wallet("open").await.unwrap();

    harness.engine.update_wallet(other.wallet.id, "renamed", other.wallet.color).await.unwrap();
    assert_eq!(
        harness.engine.lock_state_now(),
        LockState::Unlocked { wallet_id: open.wallet.id, wallet_name: "open".to_owned() }
    );
}

#[tokio::test]
async fn a_wallet_name_is_trimmed_and_never_blank() {
    let harness = harness().await.unwrap();
    let refused = harness
        .engine
        .create_seed_wallet(
            "  ",
            WalletColor::default(),
            secret(PASSWORD),
            PhraseWords::Twelve,
            PassphraseInput::None,
        )
        .await;
    assert!(matches!(refused, Err(CoreError::InvalidInput { field: "name", .. })), "{refused:?}");

    let created = harness.seed_wallet(" padded ").await.unwrap();
    assert_eq!(created.wallet.name, "padded");
    let refused =
        harness.engine.update_wallet(created.wallet.id, " ", WalletColor::default()).await;
    assert!(matches!(refused, Err(CoreError::InvalidInput { field: "name", .. })), "{refused:?}");
    assert_eq!(harness.engine.list_wallets().await.unwrap(), vec![created.wallet]);
}

#[tokio::test]
async fn deleting_needs_the_name_and_the_password() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("doomed").await.unwrap();
    let id = created.wallet.id;

    let refused = harness.engine.delete_wallet(id, secret(PASSWORD), "wrong").await;
    assert!(matches!(refused, Err(CoreError::ConfirmationRequired { .. })), "{refused:?}");

    let refused = harness.engine.delete_wallet(id, secret(OTHER_PASSWORD), "doomed").await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    assert_eq!(harness.engine.list_wallets().await.unwrap().len(), 1);

    harness.engine.delete_wallet(id, secret(PASSWORD), "doomed").await.unwrap();
    assert!(harness.engine.list_wallets().await.unwrap().is_empty());
    assert!(!harness.engine.lock_state_now().is_unlocked());
}

#[tokio::test]
async fn a_closed_wallet_is_deleted_without_touching_the_session() {
    let harness = harness().await.unwrap();
    let doomed = harness.seed_wallet("doomed").await.unwrap();
    harness.engine.lock_wallet().await;
    let open = harness.seed_wallet("open").await.unwrap();

    let refused = harness.engine.delete_wallet(doomed.wallet.id, secret("nope"), "doomed").await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");

    harness.engine.delete_wallet(doomed.wallet.id, secret(PASSWORD), "doomed").await.unwrap();
    assert_eq!(harness.engine.list_wallets().await.unwrap().len(), 1);
    assert_eq!(harness.engine.lock_state_now().wallet_id(), Some(open.wallet.id));
}

#[tokio::test]
async fn rotating_the_password_retires_the_old_one() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("rotating").await.unwrap();
    harness.engine.change_password(secret(PASSWORD), secret(OTHER_PASSWORD)).await.unwrap();
    harness.engine.lock_wallet().await;

    let refused = harness.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await;
    assert!(matches!(refused, Err(CoreError::WrongPassword)), "{refused:?}");
    harness.engine.unlock_wallet(created.wallet.id, secret(OTHER_PASSWORD), None).await.unwrap();
}

#[tokio::test]
async fn session_operations_refuse_a_locked_engine() {
    let harness = harness().await.unwrap();
    assert!(matches!(harness.engine.list_addresses(None).await, Err(CoreError::Locked)));
    assert!(matches!(harness.engine.list_enabled_chains().await, Err(CoreError::Locked)));
    assert!(matches!(harness.engine.list_balances().await, Err(CoreError::Locked)));
    assert!(matches!(test_util::list_pending_txs(&harness.engine).await, Err(CoreError::Locked)));
    assert!(matches!(harness.engine.sync_now(None).await, Err(CoreError::Locked)));
    assert!(matches!(harness.engine.touch_session().await, Err(CoreError::Locked)));
}

#[tokio::test]
async fn a_wallet_whose_vault_fails_leaves_nothing_behind() {
    let harness = harness().await.unwrap();
    // A directory where the vault file belongs is refused as a vault that already exists.
    let vault = test_util::vault_path(harness.scratch.path(), 1);
    fs::create_dir_all(&vault).unwrap();

    let refused = harness
        .engine
        .create_seed_wallet(
            "doomed",
            WalletColor::default(),
            secret(PASSWORD),
            PhraseWords::Twelve,
            PassphraseInput::None,
        )
        .await;
    assert!(
        matches!(refused, Err(CoreError::AlreadyExists { entity: "vault" })),
        "creating a wallet over a directory must fail: {refused:?}"
    );
    assert!(harness.engine.list_wallets().await.unwrap().is_empty());
    assert!(!harness.engine.lock_state_now().is_unlocked());
}

#[tokio::test]
async fn a_wallet_id_a_leftover_vault_file_still_holds_is_stepped_over() {
    let harness = harness().await.unwrap();
    let first = harness.seed_wallet("before").await.unwrap();
    let orphaned = test_util::vault_path(harness.scratch.path(), first.wallet.id);
    assert!(orphaned.is_file());

    // A rebuilt database leaves exactly this behind, a vault file without its row.
    // The engine's own delete removes both.
    let scratch = harness.shut_down().await.unwrap();
    fs::write(scratch.path().join("jelly.sqlite"), b"this is not a database").unwrap();
    let harness =
        harness_in(test_util::config(), scratch, Arc::new(MemoryKeyStore::new())).await.unwrap();
    assert!(harness.engine.list_wallets().await.unwrap().is_empty());
    assert!(orphaned.is_file());

    let created = harness.seed_wallet("after").await.unwrap();
    assert_ne!(created.wallet.id, first.wallet.id);
    assert!(orphaned.is_file(), "the orphaned vault must not be clobbered");
    assert_eq!(harness.engine.list_wallets().await.unwrap().len(), 1);

    // The wallet that was created is the one that can be opened.
    harness.engine.lock_wallet().await;
    let reopened =
        harness.engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();
    assert_eq!(reopened.id, created.wallet.id);
}

#[tokio::test]
async fn a_start_that_fails_after_replacing_the_database_still_reports_where_it_went() {
    let scratch = tempfile::tempdir().unwrap();
    fs::write(scratch.path().join("jelly.sqlite"), b"this is not a database").unwrap();
    fs::write(scratch.path().join("settings.json"), b"{ not json").unwrap();
    let deps = EngineDeps {
        data_dir: scratch.path().to_path_buf(),
        clock: Arc::new(test_util::BreakableClock::default()),
        keystore: Arc::new(MemoryKeyStore::new()),
        builder: Arc::new(test_util::NullAdapterBuilder),
        table: jelly_chains::chains(),
    };

    let failure = WalletEngine::start(test_util::config(), deps)
        .await
        .expect_err("the settings file does not parse");
    assert!(matches!(failure.error, StartupError::SettingsInvalid { .. }), "{failure:?}");
    let rebuilt = failure.rebuilt.expect("the failed start reports the database it replaced");
    assert!(
        Path::new(&rebuilt.moved_to).is_file(),
        "the unusable file is where the failure says it went"
    );
}

#[tokio::test]
async fn a_wallet_id_a_leftover_master_key_still_holds_is_stepped_over() {
    let harness = harness().await.unwrap();
    let first = harness.seed_wallet("before").await.unwrap();
    let keystore = Arc::clone(&harness.keystore);

    // A rebuilt database after the vault file was moved aside leaves only its master
    // key behind, and that key is the only one the moved file opens under.
    let scratch = harness.shut_down().await.unwrap();
    let moved = scratch.path().join("moved-aside.vault");
    fs::rename(test_util::vault_path(scratch.path(), first.wallet.id), &moved).unwrap();
    fs::write(scratch.path().join("jelly.sqlite"), b"this is not a database").unwrap();
    let harness = harness_in(test_util::config(), scratch, keystore).await.unwrap();

    let created = harness.seed_wallet("after").await.unwrap();
    assert_ne!(created.wallet.id, first.wallet.id);

    let original = Vault::new(
        moved,
        first.wallet.id.to_string(),
        Arc::<MemoryKeyStore>::clone(&harness.keystore),
        Argon2Params::MINIMUM,
    );
    original.unlock(&password(PASSWORD)).unwrap().lock();
}

#[tokio::test]
async fn a_wallet_delete_can_finish_after_database_cleanup_failed() {
    let harness = harness().await.unwrap();
    let wallet = harness.seed_wallet("doomed").await.unwrap().wallet;
    let path = test_util::vault_path(harness.scratch.path(), wallet.id);
    harness.clock.set_broken(true);

    let refused = harness.engine.delete_wallet(wallet.id, secret(PASSWORD), "doomed").await;
    assert!(refused.is_err(), "the database could not read the clock for the delete");
    assert!(!path.exists());
    assert!(!harness.keystore.contains(&wallet.id.to_string()));
    assert_eq!(harness.engine.list_wallets().await.unwrap(), vec![wallet.clone()]);
    assert!(!harness.engine.lock_state_now().is_unlocked());

    harness.clock.set_broken(false);
    let refused = harness.engine.delete_wallet(wallet.id, secret(PASSWORD), "wrong name").await;
    assert!(matches!(refused, Err(CoreError::ConfirmationRequired { .. })), "{refused:?}");
    harness.engine.delete_wallet(wallet.id, secret(""), "doomed").await.unwrap();
    assert!(harness.engine.list_wallets().await.unwrap().is_empty());
}

#[tokio::test]
async fn missing_vault_cleanup_preserves_a_moved_vaults_master_key() {
    let harness = harness().await.unwrap();
    let wallet = harness.seed_wallet("keep").await.unwrap().wallet;
    harness.engine.lock_wallet().await;
    let path = test_util::vault_path(harness.scratch.path(), wallet.id);
    let backup = path.with_extension("backup");
    fs::rename(&path, &backup).unwrap();
    let moved = Vault::new(
        &backup,
        wallet.id.to_string(),
        Arc::<MemoryKeyStore>::clone(&harness.keystore),
        Argon2Params::MINIMUM,
    );
    moved.unlock(&password(PASSWORD)).unwrap().lock();

    for supplied in [secret("wrong password"), secret(PASSWORD)] {
        let refused = harness.engine.delete_wallet(wallet.id, supplied, "keep").await;
        assert!(refused.is_err(), "a missing vault authorises nothing");
        assert!(harness.keystore.contains(&wallet.id.to_string()));
        assert_eq!(harness.engine.list_wallets().await.unwrap(), vec![wallet.clone()]);
        moved.unlock(&password(PASSWORD)).unwrap().lock();
    }

    fs::rename(backup, path).unwrap();
    harness.engine.delete_wallet(wallet.id, secret(PASSWORD), "keep").await.unwrap();
    assert!(!harness.keystore.contains(&wallet.id.to_string()));
    assert!(harness.engine.list_wallets().await.unwrap().is_empty());
}

#[tokio::test]
async fn an_unreadable_vault_does_not_authorise_cleanup() {
    let harness = harness().await.unwrap();
    let wallet = harness.seed_wallet("keep").await.unwrap().wallet;
    harness.engine.lock_wallet().await;
    let path = test_util::vault_path(harness.scratch.path(), wallet.id);
    let backup = path.with_extension("backup");
    fs::rename(&path, &backup).unwrap();
    fs::create_dir(&path).unwrap();

    let refused = harness.engine.delete_wallet(wallet.id, secret(PASSWORD), "keep").await;
    assert!(refused.is_err(), "a directory is not a vault");
    assert!(harness.keystore.contains(&wallet.id.to_string()));
    assert_eq!(harness.engine.list_wallets().await.unwrap(), vec![wallet.clone()]);

    fs::remove_dir(&path).unwrap();
    fs::write(&path, b"not a vault").unwrap();
    let refused = harness.engine.delete_wallet(wallet.id, secret(PASSWORD), "keep").await;
    assert!(refused.is_err(), "a file that does not parse is not a vault");
    assert!(harness.keystore.contains(&wallet.id.to_string()));
    fs::remove_file(&path).unwrap();
    fs::rename(backup, path).unwrap();
    harness.engine.unlock_wallet(wallet.id, secret(PASSWORD), None).await.unwrap();
}

#[tokio::test]
async fn new_wallets_and_password_rotations_require_nonempty_passwords() {
    let harness = harness().await.unwrap();
    for outcome in [
        harness
            .engine
            .create_seed_wallet(
                "empty",
                WalletColor::default(),
                secret(""),
                PhraseWords::Twelve,
                PassphraseInput::None,
            )
            .await
            .map(|created| created.wallet),
        harness
            .engine
            .import_seed_wallet(
                "empty",
                WalletColor::default(),
                secret(""),
                SecretText::new(String::new()),
                PassphraseInput::None,
            )
            .await,
    ] {
        assert!(
            matches!(outcome, Err(CoreError::InvalidInput { field: "password", .. })),
            "{outcome:?}"
        );
    }
    assert!(harness.engine.list_wallets().await.unwrap().is_empty());
    assert!(harness.keystore.is_empty());

    let wallet = harness.seed_wallet("keep").await.unwrap().wallet;
    let refused = harness.engine.change_password(secret(PASSWORD), secret("")).await;
    assert!(
        matches!(refused, Err(CoreError::InvalidInput { field: "password", .. })),
        "{refused:?}"
    );
    harness.engine.lock_wallet().await;
    harness.engine.unlock_wallet(wallet.id, secret(PASSWORD), None).await.unwrap();
}

/// A credential store whose next delete fails once, the way a platform store does when
/// it is briefly unavailable.
#[derive(Debug, Default)]
struct FailOnceDeleteKeyStore {
    store: MemoryKeyStore,
    should_fail_delete: AtomicBool,
}

impl MasterKeyStore for FailOnceDeleteKeyStore {
    fn get(&self, wallet_id: &str) -> Result<Option<MasterKey>, VaultError> {
        self.store.get(wallet_id)
    }

    fn put(&self, wallet_id: &str, key: &MasterKey) -> Result<(), VaultError> {
        self.store.put(wallet_id, key)
    }

    fn delete(&self, wallet_id: &str) -> Result<(), VaultError> {
        if self.should_fail_delete.swap(false, Ordering::SeqCst) {
            return Err(VaultError::KeychainUnavailable);
        }
        self.store.delete(wallet_id)
    }
}

#[tokio::test]
async fn a_wallet_delete_can_retry_after_a_transient_master_key_deletion_failure() {
    let keystore = Arc::new(FailOnceDeleteKeyStore::default());
    let harness = harness_with_keystore(Arc::clone(&keystore)).await.unwrap();
    let wallet = harness.seed_wallet("doomed").await.unwrap().wallet;
    let path = test_util::vault_path(harness.scratch.path(), wallet.id);
    keystore.should_fail_delete.store(true, Ordering::SeqCst);

    let error = harness.engine.delete_wallet(wallet.id, secret(PASSWORD), "doomed").await;
    assert!(matches!(error, Err(CoreError::Keychain { .. })), "{error:?}");
    assert!(!keystore.should_fail_delete.load(Ordering::SeqCst));
    assert!(keystore.store.contains(&wallet.id.to_string()));
    assert_eq!(harness.engine.list_wallets().await.unwrap(), vec![wallet.clone()]);
    assert!(!harness.engine.lock_state_now().is_unlocked());

    let scratch = harness.shut_down().await.unwrap();
    let harness = harness_in(test_util::config(), scratch, Arc::clone(&keystore)).await.unwrap();
    harness
        .engine
        .delete_wallet(wallet.id, secret(PASSWORD), "doomed")
        .await
        .expect("retrying an authorised deletion must finish after the keychain recovers");
    assert!(!path.exists());
    assert!(!keystore.store.contains(&wallet.id.to_string()));
    assert!(harness.engine.list_wallets().await.unwrap().is_empty());
}

#[tokio::test]
async fn settings_do_not_need_an_open_wallet() {
    let harness = harness().await.unwrap();
    let settings = harness.engine.app_settings().await;
    harness
        .engine
        .set_auto_lock_minutes(settings.auto_lock_minutes.saturating_add(1))
        .await
        .unwrap();
    let after = harness.engine.app_settings().await;
    assert_eq!(after.auto_lock_minutes, settings.auto_lock_minutes.saturating_add(1));
}
