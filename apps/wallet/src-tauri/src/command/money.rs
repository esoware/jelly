use jelly_core::AssetId;
use jelly_engine::{AssetTotal, BalanceRecord};
use jelly_prices::{CandleInterval, PriceChart, PriceHistory, PriceSnapshot};
use tauri::State;

use crate::command::{with_engine, with_services};
use crate::error::IpcError;
use crate::state::AppState;

#[tauri::command]
#[specta::specta]
pub(crate) async fn list_balances(
    state: State<'_, AppState>,
) -> Result<Vec<BalanceRecord>, IpcError> {
    with_engine(&state, async move |engine| engine.list_balances().await).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn list_balance_totals(
    state: State<'_, AppState>,
) -> Result<Vec<AssetTotal>, IpcError> {
    with_engine(&state, async move |engine| engine.list_balance_totals().await).await
}

/// Returns cached prices with their fetch times, and none while prices are off. Wallet
/// operations don't wait for price updates.
#[tauri::command]
#[specta::specta]
pub(crate) async fn list_prices(state: State<'_, AppState>) -> Result<PriceSnapshot, IpcError> {
    with_services(&state, async move |services| Ok(services.prices().snapshot())).await
}

/// Empty while prices are off. Each series is added as it loads.
#[tauri::command]
#[specta::specta]
pub(crate) async fn list_price_history(
    state: State<'_, AppState>,
) -> Result<PriceHistory, IpcError> {
    with_services(&state, async move |services| Ok(services.price_history().snapshot())).await
}

/// An asset's candles in the wallet's currency, fetched on first request and reused while
/// fresh. Fails while prices are off.
#[tauri::command]
#[specta::specta]
pub(crate) async fn get_price_chart(
    state: State<'_, AppState>,
    asset: AssetId,
    interval: CandleInterval,
) -> Result<PriceChart, IpcError> {
    with_services(&state, async move |services| services.price_chart(asset, interval).await).await
}
