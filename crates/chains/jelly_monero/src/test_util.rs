//! Operations on what the adapter persists and compiles in. Only
//! `[dev-dependencies]` enable the `test-util` feature.
//!
//! A test reaches the scan blob and the sends in flight through these rather than
//! through the scope, stream and slot they are stored under.

use jelly_core::{Amount, CoreError, CursorBytes, CursorScope, CursorStore, Rejection};
use monero_interface::FeeRate;

use crate::adapter::MoneroAdapter;
use crate::balance::MoneroBalance;
use crate::broadcast::BroadcastRefusal;
use crate::fee;
use crate::params::{SCAN_STREAM, SENDS_SLOT};
use crate::state::timelock_now_secs;

pub fn account_balance(adapter: &MoneroAdapter, major: u32) -> MoneroBalance {
    let guard = adapter.state().lock();
    guard.account_balance(major, guard.tip_height(), timelock_now_secs(adapter.clock()))
}

/// Transactions `adapter` broadcast and has not yet seen on chain.
pub fn in_flight(adapter: &MoneroAdapter) -> usize {
    adapter.state().lock().in_flight_count()
}

/// The scan blob the adapter last checkpointed in `cursors`.
pub async fn stored_scan(cursors: &dyn CursorStore) -> Result<Option<CursorBytes>, CoreError> {
    cursors.load_scan(CursorScope::Wallet, SCAN_STREAM).await
}

/// Overwrites the scan blob in `cursors`, as a damaged or tampered store would.
pub async fn replace_scan(cursors: &dyn CursorStore, blob: CursorBytes) -> Result<(), CoreError> {
    cursors.advance(CursorScope::Wallet, SCAN_STREAM, blob).await
}

/// The sends in flight the adapter last reserved in `cursors`.
pub async fn stored_sends(cursors: &dyn CursorStore) -> Result<Option<CursorBytes>, CoreError> {
    cursors.load_reservation(CursorScope::Wallet, SENDS_SLOT).await
}

/// The rejection the shipped node reports when monerod refuses with `reason`
/// and sets none of its refusal flags.
pub fn refusal(reason: &str) -> Rejection {
    BroadcastRefusal::classify(reason).into_rejection(reason)
}

/// The fee a payment spending `inputs` outputs pays at `rate`, estimated the way
/// a quote estimates it.
pub fn estimated_fee(rate: FeeRate, inputs: usize) -> Amount {
    fee::estimate_fee(rate, inputs)
}
