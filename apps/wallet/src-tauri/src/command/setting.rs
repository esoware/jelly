use jelly_core::{AssetId, FiatCurrency, ProxySetting, SecretText};
use jelly_engine::{AmountEntry, AppSettings, BackdropSetting};
use tauri::{AppHandle, State};

use crate::command::{with_engine, with_services};
use crate::error::IpcError;
use crate::state::AppState;
use crate::window;

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, IpcError> {
    with_engine(&state, async move |engine| Ok(engine.app_settings().await)).await
}

/// `None` removes the key. Only indexed EVM history uses it.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_etherscan_api_key(
    state: State<'_, AppState>,
    key: Option<SecretText>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_etherscan_api_key(key).await).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_trongrid_api_key(
    state: State<'_, AppState>,
    key: Option<SecretText>,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_trongrid_api_key(key).await).await
}

/// Zero turns auto-lock off. The change applies to the open session too.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_auto_lock_minutes(
    state: State<'_, AppState>,
    minutes: u32,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_auto_lock_minutes(minutes).await).await
}

/// Pauses or resumes price polling without stopping its task. Turning the feed on also
/// switches amount entry to fiat, and turning it off switches it to units.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_price_feed_enabled(
    state: State<'_, AppState>,
    is_enabled: bool,
) -> Result<(), IpcError> {
    with_services(&state, async move |services| services.set_price_feed_enabled(is_enabled).await)
        .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_price_chart_enabled(
    state: State<'_, AppState>,
    is_enabled: bool,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_price_chart_enabled(is_enabled).await).await
}

/// Prices update at once from the rates already held.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_fiat_currency(
    state: State<'_, AppState>,
    currency: FiatCurrency,
) -> Result<(), IpcError> {
    with_services(&state, async move |services| services.set_fiat_currency(currency).await).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_amount_entry(
    state: State<'_, AppState>,
    entry: AmountEntry,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_amount_entry(entry).await).await
}

/// Validates and saves the proxy, then applies it. Prices switch to it at once, even while
/// locked, and chain connections follow after a debounced adapter rebuild.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_proxy(
    state: State<'_, AppState>,
    proxy: Option<ProxySetting>,
) -> Result<(), IpcError> {
    with_services(&state, async move |services| services.set_proxy(proxy).await).await
}

/// Saves the backdrop and applies it to the window at once. A window that can't blur
/// behind itself stays solid whatever is saved.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_backdrop(
    handle: AppHandle,
    state: State<'_, AppState>,
    backdrop: BackdropSetting,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_backdrop(backdrop).await).await?;
    window::apply_backdrop(&handle, backdrop);
    Ok(())
}

/// Shows a token in its chain's balance panel in place of the native coin. A native asset
/// puts the coin back.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_shown_asset(
    state: State<'_, AppState>,
    asset: AssetId,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_shown_asset(asset).await).await
}
