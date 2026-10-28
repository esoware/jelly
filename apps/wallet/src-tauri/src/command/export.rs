use jelly_core::ChainId;
use secrecy::SecretString;
use tauri::{State, WebviewWindow};

use crate::command::{RevealedSecret, TARGET, with_engine};
use crate::error::IpcError;
use crate::state::AppState;

/// Takes the wallet password even while the wallet is open.
///
/// The phrase crosses IPC as a string we can't wipe, so the exports sit behind their own
/// capability. The frontend must protect the window while it shows the phrase and keep it
/// out of query caches.
#[tauri::command]
#[specta::specta]
pub(crate) async fn export_seed_phrase(
    state: State<'_, AppState>,
    password: String,
) -> Result<RevealedSecret, IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine.export_seed_phrase(password).await.map(RevealedSecret::from)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn export_seed_passphrase(
    state: State<'_, AppState>,
    password: String,
) -> Result<RevealedSecret, IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine.export_seed_passphrase(password).await.map(RevealedSecret::from)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn export_seed_entropy_hex(
    state: State<'_, AppState>,
    password: String,
) -> Result<RevealedSecret, IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine.export_seed_entropy_hex(password).await.map(RevealedSecret::from)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn export_account_seed(
    state: State<'_, AppState>,
    chain: ChainId,
    password: String,
) -> Result<RevealedSecret, IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine.export_account_seed(chain, password).await.map(RevealedSecret::from)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn export_private_key(
    state: State<'_, AppState>,
    address_id: i64,
    password: String,
) -> Result<RevealedSecret, IpcError> {
    let password = SecretString::from(password);
    with_engine(&state, async move |engine| {
        engine.export_private_key(address_id, password).await.map(RevealedSecret::from)
    })
    .await
}

/// Turns screen capture protection on while a secret is visible. The component that shows
/// the secret turns it off on cleanup.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_secret_screen(
    window: WebviewWindow,
    is_protected: bool,
) -> Result<(), IpcError> {
    window.set_content_protected(is_protected).map_err(|error| {
        tracing::warn!(target: TARGET, %error, is_protected, "screen capture protection was refused");
        IpcError::internal("The window could not change screen capture protection.")
    })
}
