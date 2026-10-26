use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jelly_core::{
    ChainId, CoreError, KeySource, PassphrasePolicy, PhraseWords, SecretText, WalletColor,
};
use jelly_store::{NewWallet, WalletRow};
use jelly_vault::{BlobKind, Vault, VaultPassword};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::MutexGuard;
use zeroize::Zeroizing;

use crate::chains::{enabled_chains, restore_heights};
use crate::error::blocking;
use crate::seed::{self, NewSeed, SeedSource};
use crate::session::{LockReason, LockState, Session};
use crate::{WalletEngine, derive};

/// Sub-directory of the data directory that holds one vault file per wallet.
const VAULT_DIR: &str = "vaults";

/// How many wallet ids one creation may step over before it gives up.
///
/// Each skip is a vault file or keychain master key a forgotten wallet left behind, so
/// only a machine holding hundreds of orphans reaches the bound.
const MAX_SKIPPED_IDS: usize = 256;

/// No caller offers a choice of account, so every seed wallet derives from the first.
const ACCOUNT_INDEX: u32 = 0;

/// One wallet as the interface sees it, with its passphrase policy and the chains it has
/// switched on.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletRecord {
    pub id: i64,
    pub name: String,
    pub color: WalletColor,
    pub created_at_unix_secs: i64,
    pub passphrase_policy: PassphrasePolicy,
    pub restore_heights: BTreeMap<ChainId, u64>,
    pub enabled_chains: Vec<ChainId>,
}

/// A wallet and the phrase that was generated for it. The phrase is handed out exactly
/// once, at creation, and afterwards takes a re-authenticated export.
pub struct CreatedWallet {
    pub wallet: WalletRecord,
    pub phrase: Zeroizing<String>,
}

impl fmt::Debug for CreatedWallet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedWallet").field("wallet", &self.wallet).finish_non_exhaustive()
    }
}

/// The passphrase a caller supplied, with the policy it implies. It arrives over IPC, so
/// it deserializes but never serializes.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PassphraseInput {
    None,
    #[serde(rename_all = "camelCase")]
    Stored {
        value: SecretText,
    },
    #[serde(rename_all = "camelCase")]
    Prompt {
        value: SecretText,
    },
}

impl PassphraseInput {
    pub const fn policy(&self) -> PassphrasePolicy {
        match self {
            Self::None => PassphrasePolicy::None,
            Self::Stored { .. } => PassphrasePolicy::Stored,
            Self::Prompt { .. } => PassphrasePolicy::Prompt,
        }
    }

    pub const fn value(&self) -> Option<&SecretText> {
        match self {
            Self::None => None,
            Self::Stored { value } | Self::Prompt { value } => Some(value),
        }
    }
}

/// Where the vault of `wallet_id` lives under `data_dir`. One file per wallet id, under
/// one directory.
pub(crate) fn vault_path(data_dir: &Path, wallet_id: i64) -> PathBuf {
    data_dir.join(VAULT_DIR).join(format!("wallet-{wallet_id}.vault"))
}

impl WalletEngine {
    pub async fn list_wallets(&self) -> Result<Vec<WalletRecord>, CoreError> {
        let rows = self.inner.store.list_wallets().await?;
        let mut wallets = Vec::with_capacity(rows.len());
        for row in rows {
            wallets.push(self.wallet_record(row).await?);
        }
        Ok(wallets)
    }

    /// Creates a seed wallet, leaves it open, and returns its phrase once.
    pub async fn create_seed_wallet(
        &self,
        name: &str,
        color: WalletColor,
        password: SecretString,
        words: PhraseWords,
        passphrase: PassphraseInput,
    ) -> Result<CreatedWallet, CoreError> {
        self.create_seed(name, color, password, SeedSource::Generate(words), passphrase).await
    }

    /// Imports a seed wallet from an existing BIP-39 phrase and leaves it open.
    pub async fn import_seed_wallet(
        &self,
        name: &str,
        color: WalletColor,
        password: SecretString,
        phrase: SecretText,
        passphrase: PassphraseInput,
    ) -> Result<WalletRecord, CoreError> {
        let created =
            self.create_seed(name, color, password, SeedSource::Phrase(phrase), passphrase).await?;
        Ok(created.wallet)
    }

    /// Opens a wallet. Any wallet already open is locked first.
    pub async fn unlock_wallet(
        &self,
        wallet_id: i64,
        password: SecretString,
        passphrase: Option<SecretText>,
    ) -> Result<WalletRecord, CoreError> {
        let _transition = self.begin_transition().await?;
        let row = self.inner.store.get_wallet(wallet_id).await?;
        let settings = self.inner.store.get_wallet_settings(wallet_id).await?;
        self.lock_inner(LockReason::Requested).await;

        let (vault, keys) = seed::unlock_vault_and_keys(
            self.vault_handle(wallet_id),
            VaultPassword::new(password),
            settings,
            passphrase,
            self.inner.table,
        )
        .await?;

        let session =
            Session::new(row.id, vault, keys, self.inner.clock.as_ref(), self.inner.table);
        // Checked before the session is installed, so a wrong passphrase never
        // opens anything.
        self.verify_passphrase(&session).await?;

        let record = self.wallet_record(row.clone()).await?;
        self.install_session(session, &row);
        self.publish_adapters().await;
        Ok(record)
    }

    /// Rotates the vault password. The session stays open.
    pub async fn change_password(
        &self,
        current: SecretString,
        new: SecretString,
    ) -> Result<(), CoreError> {
        let session = self.require_session().await?;
        require_new_password(&new)?;
        let current = VaultPassword::new(current);
        let new = VaultPassword::new(new);
        session.vault_write(move |vault| vault.change_password(&current, &new)).await
    }

    pub async fn update_wallet(
        &self,
        wallet_id: i64,
        name: &str,
        color: WalletColor,
    ) -> Result<(), CoreError> {
        let name = wallet_name(name)?;
        let _transition = self.begin_transition().await?;
        let row = self.inner.store.update_wallet(wallet_id, name, color).await?;
        if self.inner.session.state().wallet_id() == Some(wallet_id) {
            self.inner.session.publish(LockState::unlocked(&row));
        }
        Ok(())
    }

    /// Erases a wallet's vault file, keychain master key, and every row. The
    /// confirmation is the wallet's own name.
    pub async fn delete_wallet(
        &self,
        wallet_id: i64,
        password: SecretString,
        confirmation: &str,
    ) -> Result<(), CoreError> {
        let _transition = self.begin_transition().await?;
        let row = self.inner.store.get_wallet(wallet_id).await?;
        if confirmation != row.name {
            return Err(CoreError::ConfirmationRequired { expected: row.name });
        }
        let open_session =
            self.inner.session.current().filter(|session| session.wallet_id() == wallet_id);
        if let Some(session) = open_session {
            session.verify_password(password).await?;
            drop(session);
            self.lock_inner(LockReason::Requested).await;
        } else {
            let handle = self.vault_handle(wallet_id);
            let password = VaultPassword::new(password);
            blocking(move || handle.authorize_destruction(&password)).await?;
        }
        let handle = self.vault_handle(wallet_id);
        blocking(move || handle.destroy()).await?;
        self.inner.store.delete_wallet(wallet_id).await?;
        tracing::warn!(target: "jelly::engine::wallet", wallet_id, "wallet deleted");
        Ok(())
    }

    /// The transition lock, refusing once the engine has begun shutting down.
    async fn begin_transition(&self) -> Result<MutexGuard<'_, ()>, CoreError> {
        let transition = self.inner.session.transition.lock().await;
        if self.inner.cancel.is_cancelled() {
            return Err(CoreError::Locked);
        }
        Ok(transition)
    }

    fn vault_handle(&self, wallet_id: i64) -> Vault {
        Vault::new(
            vault_path(&self.inner.data_dir, wallet_id),
            wallet_id.to_string(),
            Arc::clone(&self.inner.keystore),
            self.inner.config.vault_kdf,
        )
    }

    /// Allocates a wallet id whose vault file and keychain master key are both absent. A
    /// rebuilt database restarts ids while old vaults and their keys may remain, possibly
    /// with the file moved elsewhere, and the vault refuses to create over either.
    ///
    /// Temporary rows occupy taken IDs until a free one is found, then are removed.
    async fn allocate_wallet(&self, new: &NewWallet) -> Result<WalletRow, CoreError> {
        let mut skipped: Vec<i64> = Vec::new();
        let outcome = loop {
            let row = match self.inner.store.create_wallet(new.clone()).await {
                Ok(row) => row,
                Err(error) => break Err(CoreError::from(error)),
            };
            let handle = self.vault_handle(row.id);
            match blocking(move || handle.is_claimed()).await {
                Ok(false) => break Ok(row),
                Ok(true) => {}
                Err(error) => {
                    skipped.push(row.id);
                    break Err(error);
                }
            }
            tracing::warn!(
                target: "jelly::engine::wallet",
                wallet_id = row.id,
                "a vault or keychain key already holds this wallet id; taking the next one"
            );
            skipped.push(row.id);
            if skipped.len() >= MAX_SKIPPED_IDS {
                break Err(CoreError::AlreadyExists { entity: "vault" });
            }
        };
        for id in skipped {
            if let Err(error) = self.inner.store.delete_wallet(id).await {
                tracing::warn!(
                    target: "jelly::engine::wallet",
                    %error,
                    "could not release a wallet id that was stepped over"
                );
            }
        }
        outcome
    }

    async fn create_seed(
        &self,
        name: &str,
        color: WalletColor,
        password: SecretString,
        source: SeedSource,
        passphrase: PassphraseInput,
    ) -> Result<CreatedWallet, CoreError> {
        require_new_password(&password)?;
        let new = NewWallet {
            name: wallet_name(name)?,
            color,
            passphrase_policy: passphrase.policy(),
            account_index: ACCOUNT_INDEX,
        };
        let (phrase, seed) =
            seed::derive_seed(source, passphrase, ACCOUNT_INDEX, self.inner.table).await?;
        let wallet = self.create_wallet(&new, VaultPassword::new(password), seed).await?;
        Ok(CreatedWallet { wallet, phrase })
    }

    /// Locks whatever is open, allocates the wallet's row, and provisions its vault,
    /// rolling the row back if that fails. The new wallet is left open.
    async fn create_wallet(
        &self,
        new: &NewWallet,
        password: VaultPassword,
        seed: NewSeed,
    ) -> Result<WalletRecord, CoreError> {
        let _transition = self.begin_transition().await?;
        self.lock_inner(LockReason::Requested).await;
        let row = self.allocate_wallet(new).await?;
        let handle = self.vault_handle(row.id);
        let NewSeed { secrets, keys } = seed;
        let vault = match seed::provision(handle.clone(), password, secrets).await {
            Ok(vault) => vault,
            Err(error) => {
                self.roll_back(handle, row.id, &error).await;
                return Err(error);
            }
        };

        let record = match self.wallet_record(row.clone()).await {
            Ok(record) => record,
            Err(error) => {
                drop(vault);
                self.roll_back(handle, row.id, &error).await;
                return Err(error);
            }
        };
        let session =
            Session::new(row.id, vault, keys, self.inner.clock.as_ref(), self.inner.table);
        self.install_session(session, &row);
        self.publish_adapters().await;
        Ok(record)
    }

    /// Rebuilds the adapter set for a freshly opened session. A failure here leaves the
    /// wallet usable and is logged, never reported as an unlock failure.
    async fn publish_adapters(&self) {
        if let Err(error) = self.rebuild_adapters().await {
            tracing::warn!(
                target: "jelly::engine::wallet",
                %error,
                "could not build the adapter set"
            );
        }
    }

    /// A wrong passphrase opens a different wallet, so the keychain is checked against
    /// something the wallet already committed to, the first derived address if there is
    /// one and the creation-time verifier otherwise.
    async fn verify_passphrase(&self, session: &Session) -> Result<(), CoreError> {
        let rows = self.inner.store.list_addresses(session.wallet_id(), None).await?;
        let derived = rows.iter().find(|row| matches!(row.source, KeySource::Derived { .. }));
        if let Some(row) = derived {
            let expected = derive::expected_address(session, row)?;
            return if expected == row.address { Ok(()) } else { Err(CoreError::WrongPassphrase) };
        }
        let stored =
            session.vault_read(|vault| Ok(vault.read_first(BlobKind::PassphraseVerifier)))?;
        let Some(stored) = stored else {
            return Ok(());
        };
        let is_match =
            stored.with_bytes(|tag| seed::is_verifier_match(session.keychain().seed(), tag));
        if is_match { Ok(()) } else { Err(CoreError::WrongPassphrase) }
    }

    /// Undoes a wallet whose vault could not be created, so a failed creation
    /// leaves no row behind. Best effort, because the caller is already returning a
    /// failure and must not mask it.
    ///
    /// A create refused as already existing wrote nothing, and the file or keychain key
    /// that refused it belongs to another vault, so only the row is removed.
    async fn roll_back(&self, handle: Vault, wallet_id: i64, error: &CoreError) {
        if !matches!(error, CoreError::AlreadyExists { .. })
            && let Err(error) = blocking(move || handle.destroy()).await
        {
            tracing::warn!(
                target: "jelly::engine::wallet",
                %error,
                "could not remove the vault of a half-created wallet"
            );
        }
        if let Err(error) = self.inner.store.delete_wallet(wallet_id).await {
            tracing::warn!(
                target: "jelly::engine::wallet",
                %error,
                "could not remove the row of a half-created wallet"
            );
        }
    }

    async fn wallet_record(&self, row: WalletRow) -> Result<WalletRecord, CoreError> {
        let settings = self.inner.store.get_wallet_settings(row.id).await?;
        let states = self.inner.store.list_chain_states(row.id).await?;
        Ok(WalletRecord {
            id: row.id,
            name: row.name,
            color: row.color,
            created_at_unix_secs: row.created_at_unix_secs,
            passphrase_policy: settings.passphrase_policy,
            restore_heights: restore_heights(&states),
            enabled_chains: enabled_chains(&states),
        })
    }
}

fn require_new_password(password: &SecretString) -> Result<(), CoreError> {
    if password.expose_secret().is_empty() {
        return Err(CoreError::invalid_input("password", "Enter a password."));
    }
    Ok(())
}

fn wallet_name(name: &str) -> Result<String, CoreError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CoreError::invalid_input("name", "Name the wallet."));
    }
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use jelly_vault::{MasterKey, MasterKeyStore, VaultError, test_util::MemoryKeyStore};
    use parking_lot::Mutex;
    use tokio::sync::Notify;
    use tokio::time::timeout;

    use super::*;
    use crate::EngineDeps;
    use crate::fixtures::{deps, harness, password, seed_wallet};
    use crate::test_util::config;

    #[tokio::test]
    async fn editing_waits_for_a_lock_transition_and_keeps_the_wallet_locked() {
        let harness = harness().await;
        let engine = &harness.engine;
        let wallet = seed_wallet(engine, "original").await;
        let transition = engine.inner.session.transition.lock().await;
        let mut editing = Box::pin(engine.update_wallet(wallet.id, "renamed", wallet.color));
        timeout(Duration::from_millis(100), &mut editing)
            .await
            .expect_err("editing waits for the transition");

        engine.lock_inner(LockReason::Requested).await;
        drop(transition);
        editing.await.unwrap();
        assert_eq!(engine.inner.store.get_wallet(wallet.id).await.unwrap().name, "renamed");
        assert!(!engine.lock_state_now().is_unlocked());
        assert!(engine.inner.session.current().is_none());
    }

    /// A credential store that holds the first key it is handed until the test releases
    /// it, so the test can act while a vault is being provisioned.
    #[derive(Debug)]
    struct PausedKeyStore {
        store: MemoryKeyStore,
        written: Notify,
        release: Mutex<Option<mpsc::Receiver<()>>>,
    }

    impl MasterKeyStore for PausedKeyStore {
        fn get(&self, wallet_id: &str) -> Result<Option<MasterKey>, VaultError> {
            self.store.get(wallet_id)
        }

        fn put(&self, wallet_id: &str, key: &MasterKey) -> Result<(), VaultError> {
            self.store.put(wallet_id, key)?;
            let receiver = self.release.lock().take();
            if let Some(receiver) = receiver {
                self.written.notify_one();
                receiver.recv().map_err(|_| VaultError::KeychainUnavailable)?;
            }
            Ok(())
        }

        fn delete(&self, wallet_id: &str) -> Result<(), VaultError> {
            self.store.delete(wallet_id)
        }
    }

    #[tokio::test]
    async fn a_record_read_failure_rolls_back_the_new_vault() {
        let directory = tempfile::tempdir().unwrap();
        let (sender, receiver) = mpsc::channel();
        let keystore = Arc::new(PausedKeyStore {
            store: MemoryKeyStore::new(),
            written: Notify::new(),
            release: Mutex::new(Some(receiver)),
        });
        let deps = EngineDeps {
            keystore: Arc::<PausedKeyStore>::clone(&keystore),
            ..deps(directory.path())
        };
        let engine = WalletEngine::start(config(), deps).await.unwrap().engine;
        let creating = {
            let engine = engine.clone();
            tokio::spawn(async move {
                engine
                    .create_seed_wallet(
                        "doomed",
                        WalletColor::default(),
                        password(),
                        PhraseWords::Twelve,
                        PassphraseInput::None,
                    )
                    .await
            })
        };
        keystore.written.notified().await;
        engine.inner.store.delete_wallet(1).await.unwrap();
        sender.send(()).unwrap();
        assert!(matches!(creating.await.unwrap(), Err(CoreError::NotFound { .. })));
        assert!(!vault_path(directory.path(), 1).exists());
        assert!(keystore.store.is_empty());
        assert!(engine.list_wallets().await.unwrap().is_empty());
        assert!(!engine.lock_state_now().is_unlocked());
    }
}
