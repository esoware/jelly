use jelly_core::ChainId;
use jelly_engine::ContactRecord;
use tauri::State;

use crate::command::with_engine;
use crate::error::IpcError;
use crate::state::AppState;

/// The open wallet's address book, in chain then address order.
#[tauri::command]
#[specta::specta]
pub(crate) async fn list_contacts(
    state: State<'_, AppState>,
) -> Result<Vec<ContactRecord>, IpcError> {
    with_engine(&state, async move |engine| engine.list_contacts().await).await
}

/// Refuses an address the chain does not accept and saves the canonical spelling of one
/// it does.
#[tauri::command]
#[specta::specta]
pub(crate) async fn add_contact(
    state: State<'_, AppState>,
    chain: ChainId,
    address: String,
    name: String,
    note: Option<String>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| {
        engine.add_contact(chain, &address, &name, note.as_deref()).await.map(|_| ())
    })
    .await
}

/// The contact keeps the chain it was saved for.
#[tauri::command]
#[specta::specta]
pub(crate) async fn update_contact(
    state: State<'_, AppState>,
    contact_id: i64,
    address: String,
    name: String,
    note: Option<String>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| {
        engine.update_contact(contact_id, &address, &name, note.as_deref()).await.map(|_| ())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_contact(
    state: State<'_, AppState>,
    contact_id: i64,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.delete_contact(contact_id).await).await
}
