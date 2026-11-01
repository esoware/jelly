use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use jelly_core::{
    AssetId, BalanceUpdate, ChainSyncProgress, CursorBytes, CursorScope, EventSink, TxConfirmation,
    WalletEvent,
};

use crate::adapter::UtxoAdapter;
use crate::adapter::state::ChainState;
use crate::adapter::watched::Watched;
use crate::cursor::SyncCursor;
use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::pass::{AddressPass, PendingTx, WatchedScript};
use crate::utxo::Utxo;

const TARGET: &str = "jelly::utxo::sync";

impl UtxoAdapter {
    /// Never advances a cursor past records that haven't been emitted. Each cursor is
    /// written after its address's records, and none is written unless the Electrum pass
    /// succeeds.
    ///
    /// `pending` holds the mempool transactions the last pass decoded, by address.
    pub(super) async fn run_pass(
        &self,
        watched: &[Watched],
        pending: &BTreeMap<i64, Arc<[PendingTx]>>,
        events: &dyn EventSink,
    ) -> Result<ChainSyncProgress, UtxoError> {
        let params = &self.params;
        let mut stored = self.load_cursors(watched).await?;
        let scripts: Vec<WatchedScript<'_>> = watched
            .iter()
            .map(|entry| WatchedScript {
                address_id: entry.address_id,
                script: &entry.script,
                cursor: stored.remove(&entry.address_id),
                pending: pending.get(&entry.address_id).map_or(&[], |decoded| &**decoded),
            })
            .collect();

        let served = self.client.pass(params, &scripts, events).await?;
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
                self.store_cursor(pass.address_id, &pass.cursor).await?;
            }
            state.record(pass.address_id, pass.coins, &pass.history, pass.pending);
        }

        // A claim lasts only while the coins it found unspent stay unspent. Once one
        // is gone, the transaction confirmed or was replaced.
        self.claims.retain_live(&live);
        *self.state.write() = state;
        Ok(progress)
    }

    /// The stored cursors of `watched`, by address, in one read.
    ///
    /// A cursor that doesn't decode is [`UtxoError::UnreadableState`], not a missing one,
    /// since its pending bookkeeping can't be rebuilt from history.
    pub(super) async fn load_cursors<'a>(
        &self,
        watched: impl IntoIterator<Item = &'a Watched>,
    ) -> Result<HashMap<i64, SyncCursor>, UtxoError> {
        let scopes: Vec<CursorScope> =
            watched.into_iter().map(|entry| CursorScope::Address(entry.address_id)).collect();
        if scopes.is_empty() {
            return Ok(HashMap::new());
        }
        let rows = self
            .cursors
            .load_scans(&scopes, &[self.params.scan_stream])
            .await
            .map_err(UtxoError::Cursor)?;
        rows.into_iter()
            .map(|row| {
                row.scope
                    .address_id()
                    .zip(row.cursor.with_bytes(SyncCursor::decode))
                    .ok_or(UtxoError::UnreadableState)
            })
            .collect()
    }

    pub(super) async fn store_cursor(
        &self,
        address_id: i64,
        cursor: &SyncCursor,
    ) -> Result<(), UtxoError> {
        self.cursors
            .advance(
                CursorScope::Address(address_id),
                self.params.scan_stream,
                CursorBytes::new(cursor.encode()?),
            )
            .await
            .map_err(UtxoError::Cursor)
    }
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
