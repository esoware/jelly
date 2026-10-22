use jelly_core::{PhraseWords, SecretText, WalletColor};
use jelly_engine::{LockState, PassphraseInput, WalletRecord};
use secrecy::SecretString;
use serde::Serialize;
use specta::Type;
use tauri::State;

use crate::command::{RevealedSecret, with_engine};
use crate::error::IpcError;
use crate::state::AppState;

/// A new seed wallet and the phrase generated for it.
///
/// The phrase crosses the boundary only here, because the interface has to show it
/// before the wallet is usable. Crossing leaves copies outside its zeroizing buffer, so
/// every later read is a re-authenticated export and the interface must not cache this.
#[derive(Debug, Serialize, Type)]
pub(crate) struct CreatedWallet {
    wallet: WalletRecord,
    phrase: RevealedSecret,
}

impl From<jelly_engine::CreatedWallet> for CreatedWallet {
    fn from(created: jelly_engine::CreatedWallet) -> Self {
        Self { wallet: created.wallet, phrase: RevealedSecret::from(created.phrase) }
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn list_wallets(
    state: State<'_, AppState>,
) -> Result<Vec<WalletRecord>, IpcError> {
    with_engine(&state, async move |engine| engine.list_wallets().await).await
}

/// The phrase in the answer crosses the boundary once. Every later read is a
/// re-authenticated export.
#[tauri::command]
#[specta::specta]
pub(crate) async fn create_seed_wallet(
    state: State<'_, AppState>,
    name: String,
    color: WalletColor,
    password: String,
    words: PhraseWords,
    passphrase: PassphraseInput,
) -> Result<CreatedWallet, IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine
            .create_seed_wallet(&name, color, password, words, passphrase)
            .await
            .map(CreatedWallet::from)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn import_seed_wallet(
    state: State<'_, AppState>,
    name: String,
    color: WalletColor,
    password: String,
    phrase: String,
    passphrase: PassphraseInput,
) -> Result<(), IpcError> {
    let password = SecretString::from(password);
    let phrase = SecretText::new(phrase);
    with_engine(&state, async move |engine| {
        engine.import_seed_wallet(&name, color, password, phrase, passphrase).await.map(|_| ())
    })
    .await
}

/// Any wallet already open is locked first, because the engine keeps one session.
#[tauri::command]
#[specta::specta]
pub(crate) async fn unlock_wallet(
    state: State<'_, AppState>,
    wallet_id: i64,
    password: String,
    passphrase: Option<String>,
) -> Result<(), IpcError> {
    let password = SecretString::from(password);
    let passphrase = passphrase.map(SecretText::new);
    with_engine(&state, async move |engine| {
        engine.unlock_wallet(wallet_id, password, passphrase).await.map(|_| ())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn lock_wallet(state: State<'_, AppState>) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| {
        engine.lock_wallet().await;
        Ok(())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn change_password(
    state: State<'_, AppState>,
    current_password: String,
    new_password: String,
) -> Result<(), IpcError> {
    let current_password = SecretString::from(current_password);
    let new_password = SecretString::from(new_password);
    with_engine(&state, async move |engine| {
        engine.change_password(current_password, new_password).await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn rename_wallet(
    state: State<'_, AppState>,
    wallet_id: i64,
    name: String,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| {
        engine.rename_wallet(wallet_id, &name).await.map(|_| ())
    })
    .await
}

/// Irreversible. `confirmation` is the wallet's own name.
#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_wallet(
    state: State<'_, AppState>,
    wallet_id: i64,
    password: String,
    confirmation: String,
) -> Result<(), IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine.delete_wallet(wallet_id, password, &confirmation).await
    })
    .await
}

/// Advisory only. Anything irreversible re-reads the real state rather than trusting
/// this.
#[tauri::command]
#[specta::specta]
pub(crate) async fn get_lock_state(state: State<'_, AppState>) -> Result<LockState, IpcError> {
    with_engine(&state, async move |engine| Ok(engine.lock_state_now())).await
}

/// Resets the auto-lock countdown. No other command does, so the interface calls this
/// for user input.
#[tauri::command]
#[specta::specta]
pub(crate) async fn touch_session(state: State<'_, AppState>) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.touch_session().await).await
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use jelly_core::{PassphrasePolicy, WalletColor};
    use jelly_engine::WalletRecord;
    use zeroize::Zeroizing;

    use super::CreatedWallet;

    const PHRASE: &str =
        "legal winner thank year wave sausage worth useful legal winner thank yellow";

    fn created() -> jelly_engine::CreatedWallet {
        jelly_engine::CreatedWallet {
            wallet: WalletRecord {
                id: 1,
                name: String::from("first"),
                color: WalletColor::default(),
                created_at_unix_secs: 0,
                passphrase_policy: PassphrasePolicy::None,
                restore_heights: BTreeMap::new(),
                enabled_chains: Vec::new(),
            },
            phrase: Zeroizing::new(String::from(PHRASE)),
        }
    }

    #[test]
    fn debug_of_a_created_wallet_leaves_out_its_phrase() {
        let rendered = format!("{:?}", CreatedWallet::from(created()));
        assert!(rendered.contains("first"), "the wallet is still shown");
        assert!(!rendered.contains("sausage"), "the seed phrase reached a debug rendering");
    }

    #[test]
    fn a_created_wallet_serializes_its_moved_phrase_and_wallet() {
        let created = created();
        let allocation = created.phrase.as_ptr();
        let created = CreatedWallet::from(created);
        assert_eq!(created.phrase.0.as_ptr(), allocation, "the phrase allocation is moved");
        let serialized = serde_json::to_value(&created).unwrap();
        assert_eq!(serialized["phrase"], PHRASE);
        assert_eq!(serialized["wallet"]["name"], "first");
    }
}
