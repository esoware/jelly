use jelly_core::{AssetId, ChainId, FiatCurrency, ProxySetting, SecretText, TokenId};
use jelly_engine::{AmountEntry, BackdropSetting};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, State};

use crate::command::{with_engine, with_services};
use crate::error::IpcError;
use crate::state::AppState;
use crate::window;

/// Application settings as the interface sees them, with whether each API key is set
/// but never the key.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate setting or key the user turns on alone, not a state of one thing"
)]
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppSettings {
    auto_lock_minutes: u32,
    has_etherscan_api_key: bool,
    has_trongrid_api_key: bool,
    is_price_feed_enabled: bool,
    /// Charts are hidden while the price feed is off, whatever this says.
    is_price_chart_enabled: bool,
    proxy: Option<ProxySetting>,
    backdrop: BackdropSetting,
    /// The chain the wallet page opens on. `None` until an asset is first shown.
    shown_chain: Option<ChainId>,
    /// At most one per chain. A chain with none shows its native coin.
    shown_tokens: Vec<TokenId>,
    fiat_currency: FiatCurrency,
    /// Amounts are entered in units while the price feed is off, whatever this says.
    amount_entry: AmountEntry,
}

impl From<jelly_engine::AppSettings> for AppSettings {
    fn from(settings: jelly_engine::AppSettings) -> Self {
        Self {
            auto_lock_minutes: settings.auto_lock_minutes,
            has_etherscan_api_key: settings.etherscan_api_key.is_some(),
            has_trongrid_api_key: settings.trongrid_api_key.is_some(),
            is_price_feed_enabled: settings.is_price_feed_enabled,
            is_price_chart_enabled: settings.is_price_chart_enabled,
            proxy: settings.proxy,
            backdrop: settings.backdrop,
            shown_chain: settings.shown_chain,
            shown_tokens: settings.shown_tokens.into_iter().collect(),
            fiat_currency: settings.fiat_currency,
            amount_entry: settings.amount_entry,
        }
    }
}

/// Returns API key presence, never key values.
#[tauri::command]
#[specta::specta]
pub(crate) async fn get_settings(state: State<'_, AppState>) -> Result<AppSettings, IpcError> {
    with_engine(&state, async move |engine| Ok(AppSettings::from(engine.app_settings().await)))
        .await
}

/// `None` removes the EVM history API key. Other chain operations do not need it.
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

/// Zero disables auto-lock. Changes apply to the current session.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_auto_lock_minutes(
    state: State<'_, AppState>,
    minutes: u32,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_auto_lock_minutes(minutes).await).await
}

/// Pauses or resumes polling without stopping the price-feed task. Also enters amounts in
/// fiat when the feed comes on and in units when it goes off.
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

/// Validates and saves the proxy before applying it. Prices use it immediately, even while
/// locked. The engine debounces adapter rebuilds to update chain connections.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_proxy(
    state: State<'_, AppState>,
    proxy: Option<ProxySetting>,
) -> Result<(), IpcError> {
    with_services(&state, async move |services| services.set_proxy(proxy).await).await
}

/// Saves the backdrop and applies it to the window at once. A window that cannot blur
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

/// Shows a token in its chain's balance panel in place of the native coin, and makes its
/// chain the one the wallet page opens on. A native asset puts the coin back.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_shown_asset(
    state: State<'_, AppState>,
    asset: AssetId,
) -> Result<(), IpcError> {
    with_engine(&state, async move |engine| engine.set_shown_asset(asset).await).await
}
