//! Runs sync using the current watch set and cursors. Emits an address's records before
//! advancing its cursor, then reconciles claimed coins against the verified unspent set.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use jelly_core::{
    AssetId, BalanceUpdate, ChainSyncProgress, CursorBytes, CursorScope, CursorStore, EventSink,
    TxConfirmation, WalletEvent,
};

use crate::adapter::client::UtxoClient;
use crate::adapter::state::ChainState;
use crate::adapter::watched::Watched;
use crate::claim::OutpointClaims;
use crate::coin::Utxo;
use crate::cursor::SyncCursor;
use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::sync::{AddressPass, PendingTx, WatchedScript};

const TARGET: &str = "jelly::utxo::sync";

#[derive(Debug, Clone)]
pub(crate) struct SyncOutcome {
    pub(crate) progress: ChainSyncProgress,
    pub(crate) state: ChainState,
}

/// Never advances a cursor past data the interface did not receive.
///
/// A cursor is written only after its address's records were emitted, and
/// nothing is written until the Electrum pass succeeds, so a pass that fails
/// there leaves every stored cursor intact.
///
/// `pending` holds the mempool transactions the last pass decoded, by address.
pub(crate) async fn run(
    params: &UtxoParams,
    client: &UtxoClient,
    cursors: &dyn CursorStore,
    claims: &OutpointClaims,
    watched: &[Watched],
    pending: &BTreeMap<i64, Arc<[PendingTx]>>,
    events: &dyn EventSink,
) -> Result<SyncOutcome, UtxoError> {
    let mut stored = load_cursors(params, cursors, watched).await?;
    let scripts: Vec<WatchedScript<'_>> = watched
        .iter()
        .map(|entry| WatchedScript {
            address_id: entry.address_id,
            script: &entry.script,
            cursor: stored.remove(&entry.address_id),
            pending: pending.get(&entry.address_id).map_or(&[], |decoded| &**decoded),
        })
        .collect();

    let served = client.pass(params, &scripts, events).await?;
    let progress = served.progress(params);
    let live = served.live_outpoints();
    let mut state = ChainState::at_tip(served.tip_height);

    for pass in served.addresses {
        for refused in &pass.refused {
            tracing::warn!(
                target: TARGET,
                chain = params.chain.as_str(),
                address_id = pass.address_id,
                detail = %refused,
                "an unspent entry the server listed was dropped"
            );
        }
        publish(params, events, &pass).await?;
        if !pass.is_unchanged {
            cursors
                .advance(
                    CursorScope::Address(pass.address_id),
                    params.scan_stream,
                    CursorBytes::new(pass.cursor.encode()?),
                )
                .await
                .map_err(UtxoError::Cursor)?;
        }
        state.record(pass.address_id, pass.coins, &pass.history, pass.pending);
    }

    // A claim lasts only while the coins it found unspent stay unspent. Once one
    // is gone, the transaction confirmed or was replaced.
    claims.retain_live(&live);
    Ok(SyncOutcome { progress, state })
}

/// The stored cursors of `watched`, by address, in one read.
///
/// A cursor that does not decode is [`UtxoError::UnreadableState`] rather than no
/// cursor at all, because the pending bookkeeping it holds cannot be rebuilt from
/// history.
pub(crate) async fn load_cursors<'a>(
    params: &UtxoParams,
    cursors: &dyn CursorStore,
    watched: impl IntoIterator<Item = &'a Watched>,
) -> Result<HashMap<i64, SyncCursor>, UtxoError> {
    let scopes: Vec<CursorScope> =
        watched.into_iter().map(|entry| CursorScope::Address(entry.address_id)).collect();
    if scopes.is_empty() {
        return Ok(HashMap::new());
    }
    let rows =
        cursors.load_scans(&scopes, &[params.scan_stream]).await.map_err(UtxoError::Cursor)?;
    rows.into_iter()
        .map(|row| {
            row.scope
                .address_id()
                .zip(row.cursor.with_bytes(SyncCursor::decode))
                .ok_or(UtxoError::UnreadableState)
        })
        .collect()
}

/// Emits the lossless records first and the coalesced balance last, the order
/// the pump expects.
///
/// An unchanged address has no records but reports its balance all the same,
/// because a full queue drops what `try_emit` offers and the history that has not
/// moved would never send it again.
async fn publish(
    params: &UtxoParams,
    events: &dyn EventSink,
    pass: &AddressPass,
) -> Result<(), UtxoError> {
    let balance = Utxo::total(&pass.coins)?;
    for record in &pass.records {
        events.emit(WalletEvent::TxDetected(record.clone())).await;
        if record.confirmations > 0 && record.confirmations <= params.confirmation_watch_depth {
            events
                .emit(WalletEvent::TxConfirmed(TxConfirmation {
                    txid: record.txid.clone(),
                    confirmations: record.confirmations,
                }))
                .await;
        }
    }
    events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
        address_id: pass.address_id,
        asset: AssetId::Native(params.chain),
        balance,
    }));
    Ok(())
}
