use jelly_core::ChainId;
use tauri::State;

use crate::command::with_engine;
use crate::error::IpcError;
use crate::state::AppState;

/// Wakes the sync loop of every chain with an adapter and returns their IDs. It does not
/// start overlapping passes.
#[tauri::command]
#[specta::specta]
pub(crate) async fn sync_now(state: State<'_, AppState>) -> Result<Vec<ChainId>, IpcError> {
    with_engine(&state, async move |engine| engine.sync_now(None).await).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn list_enabled_chains(
    state: State<'_, AppState>,
) -> Result<Vec<ChainId>, IpcError> {
    with_engine(&state, async move |engine| engine.list_enabled_chains().await).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_chain_enabled(
    state: State<'_, AppState>,
    chain: ChainId,
    is_enabled: bool,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_chain_enabled(chain, is_enabled).await).await
}

/// `None` restores the shipped interval for the chain.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_poll_interval(
    state: State<'_, AppState>,
    chain: ChainId,
    poll_secs: Option<u32>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_poll_interval(chain, poll_secs).await).await
}

/// Sets the block the chain's scan starts from. `None` clears it, so the scan starts from
/// block zero.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_restore_height(
    state: State<'_, AppState>,
    chain: ChainId,
    height: Option<u64>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_restore_height(chain, height).await).await
}
