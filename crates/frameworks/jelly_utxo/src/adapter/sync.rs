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
use crate::cursor::SyncCursor;
use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::sync::{AddressPass, PendingTx, WatchedScript};
use crate::utxo::Utxo;

const TARGET: &str = "jelly::utxo::sync";

#[derive(Debug, Clone)]
pub(crate) struct SyncOutcome {
    pub(crate) progress: ChainSyncProgress,
    pub(crate) state: ChainState,
}

/// Never advances a cursor past records that haven't been emitted. Each cursor is
/// written after its address's records, and none is written unless the Electrum pass
/// succeeds.
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
/// A cursor that doesn't decode is [`UtxoError::UnreadableState`], not a missing one,
/// since its pending bookkeeping can't be rebuilt from history.
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

/// Emits the records first and the balance last, the order the pump expects.
///
/// An unchanged address still reports its balance. `try_emit` drops a report when the
/// queue is full, and an unchanged history would never send it again.
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
