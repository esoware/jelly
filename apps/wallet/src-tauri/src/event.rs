use jelly_core::{AssetId, TxId, WalletEvent};
use jelly_engine::LockState;
use jelly_prices::{PriceHistory, PriceSnapshot};
use serde::Serialize;
use specta::Type;
use tauri::AppHandle;
use tauri_specta::Event;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, watch};

use crate::services::Services;
use crate::state::{Phase, StartupStatus};
use crate::window::MAIN_WINDOW;

const TARGET: &str = "jelly::app::event";

// Each event's wire name is its type name in kebab-case. Payloads derive only
// `Serialize`, so none can become a command argument.

/// An address's balance changed. The frontend refetches balances, so the event carries
/// nothing.
#[derive(Debug, Clone, Serialize, Type, Event)]
pub(crate) struct BalanceChanged;

/// A transaction the wallet hadn't recorded before. The history already holds every row
/// its sync batch wrote, so the frontend reads the wallet's view of it from there.
#[derive(Debug, Clone, Serialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TxDetected {
    txid: TxId,
    asset: AssetId,
}

/// A transaction the wallet already recorded changed. The frontend refetches transactions,
/// so the event carries nothing.
#[derive(Debug, Clone, Serialize, Type, Event)]
pub(crate) struct TxUpdated;

/// A transaction's confirmation count changed. The frontend refetches transactions, so
/// the event carries nothing.
#[derive(Debug, Clone, Serialize, Type, Event)]
pub(crate) struct TxConfirmed;

#[derive(Debug, Clone, Serialize, Type, Event)]
pub(crate) struct LockChanged(LockState);

#[derive(Debug, Clone, Serialize, Type, Event)]
pub(crate) struct StartupChanged(StartupStatus);

/// Every price in the currency the wallet shows, sent again when the currency changes.
#[derive(Debug, Clone, Serialize, Type, Event)]
pub(crate) struct PricesChanged(PriceSnapshot);

#[derive(Debug, Clone, Serialize, Type, Event)]
pub(crate) struct PriceHistoryChanged(PriceHistory);

/// Sends to the wallet window. The page lacks `core:event:allow-emit`, so it can't forge
/// these.
fn emit<E>(handle: &AppHandle, event: &E)
where
    E: Event + Serialize + Clone,
{
    if let Err(error) = event.emit_to(handle, MAIN_WINDOW) {
        tracing::warn!(target: TARGET, name = E::NAME, %error, "an event didn't reach the window");
    }
}

/// Forwards startup status until a terminal phase. Startup owns this task.
pub(crate) async fn startup_pump(handle: AppHandle, mut phases: watch::Receiver<Phase>) {
    loop {
        let phase = phases.borrow_and_update().clone();
        let is_terminal = phase.is_terminal();
        emit(&handle, &StartupChanged(phase.status()));
        if is_terminal || phases.changed().await.is_err() {
            return;
        }
    }
}

/// Starts event forwarding after startup. The services tracker cancels and drains these
/// tasks before engine shutdown.
pub(crate) fn spawn_pumps(handle: &AppHandle, services: &Services) {
    let Ok(engine) = services.engine() else {
        return;
    };

    services.spawn(wallet_events(handle.clone(), engine.subscribe()));
    services.spawn(bridge(handle.clone(), engine.lock_state(), LockChanged));
    services.spawn(bridge(handle.clone(), services.prices().subscribe(), |book| {
        PricesChanged(book.snapshot())
    }));
    services.spawn(bridge(
        handle.clone(),
        services.price_history().subscribe(),
        PriceHistoryChanged,
    ));
}

/// Sends the current value, then every change, empty ones included so the frontend clears
/// stale state.
async fn bridge<T, E>(
    handle: AppHandle,
    mut values: watch::Receiver<T>,
    wrap: impl Fn(T) -> E + Send,
) where
    T: Clone + Send + Sync + 'static,
    E: Event + Serialize + Clone,
{
    loop {
        let current = values.borrow_and_update().clone();
        emit(&handle, &wrap(current));
        if values.changed().await.is_err() {
            return;
        }
    }
}

/// Falling behind loses refresh hints instead of blocking sync, and the store stays
/// authoritative.
async fn wallet_events(handle: AppHandle, mut events: broadcast::Receiver<WalletEvent>) {
    loop {
        match events.recv().await {
            Ok(event) => forward(&handle, event),
            Err(RecvError::Lagged(dropped)) => {
                tracing::warn!(target: TARGET, dropped, "the interface fell behind the events");
            }
            Err(RecvError::Closed) => return,
        }
    }
}

fn forward(handle: &AppHandle, event: WalletEvent) {
    match event {
        WalletEvent::BalanceChanged(_) => emit(handle, &BalanceChanged),
        WalletEvent::TxDetected(record) => {
            emit(handle, &TxDetected { txid: record.txid, asset: record.asset });
        }
        WalletEvent::TxUpdated(_) => emit(handle, &TxUpdated),
        WalletEvent::TxConfirmed(_) => emit(handle, &TxConfirmed),
        // Nothing in the frontend reads sync progress.
        WalletEvent::SyncProgress(_) => {}
    }
}
