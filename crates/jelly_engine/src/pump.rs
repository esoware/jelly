use std::collections::BTreeMap;
use std::mem::take;
use std::sync::Weak;

use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainId, ChainSyncProgress, CoreError, CursorBytes,
    CursorScope, ScanAdvance, ScanStream, TxConfirmation, TxId, TxRecord, WalletEvent,
};
use jelly_store::{SyncBatch, TxWrite};
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;

use crate::config::EngineConfig;
use crate::engine::Inner;
use crate::events::{Events, Inbound, announcement};
use crate::secret_cursor::write_secret;

const TARGET: &str = "jelly::engine::events";

/// Database-backed scan cursors, one per scope and stream.
type Scans = BTreeMap<(CursorScope, ScanStream), CursorBytes>;

/// Vault-backed scan cursors, one per scope and stream.
type SecretScans = BTreeMap<(CursorScope, ScanStream), SecretCursor>;

/// A vault-backed cursor with the wallet it was produced under.
#[derive(Debug)]
struct SecretCursor {
    wallet_id: i64,
    cursor: CursorBytes,
}

/// What a failed window keeps for retry. Progress isn't kept, because the next pass
/// reports it again.
#[derive(Debug, Default)]
struct Carry {
    events: Vec<WalletEvent>,
    balances: Vec<BalanceUpdate>,
    scans: Scans,
    secrets: SecretScans,
}

impl Carry {
    fn is_empty(&self) -> bool {
        self.events.is_empty()
            && self.balances.is_empty()
            && self.scans.is_empty()
            && self.secrets.is_empty()
    }

    /// Keeps the newest `capacity` events and balances. Losing an event also drops the
    /// carried cursors and counts a loss, which refuses the cursors still queued behind
    /// it, so the next pass rescans from the last committed position.
    fn bounded(mut self, capacity: usize, events: &Events) -> Self {
        let has_lost = drop_oldest(&mut self.events, capacity);
        drop_oldest(&mut self.balances, capacity);
        if has_lost {
            self.scans.clear();
            self.secrets.clear();
            events.record_loss();
        }
        self
    }

    /// Gives up on a window the database refused `attempts` times.
    ///
    /// Losing rows counts a loss, which refuses the advances still queued behind them,
    /// and forgets the reconstructible scans of their addresses so the next pass rescans
    /// them. Other streams keep their last committed checkpoint for the next pass to
    /// reconcile.
    async fn abandon(self, inner: &Inner, attempts: u32) {
        tracing::error!(
            target: TARGET,
            attempts,
            events = self.events.len(),
            "giving up on a window of sync results the database refused"
        );
        if !self.events.is_empty() {
            inner.events.record_loss();
        }
        let address_ids = self.address_ids();
        if !address_ids.is_empty()
            && let Err(error) = inner.store.forget_reconstructible_scans(address_ids).await
        {
            tracing::error!(
                target: TARGET,
                %error,
                "the scan progress of a dropped window could not be forgotten"
            );
        }
    }

    /// Every address whose detected transactions this carry still holds.
    fn address_ids(&self) -> Vec<i64> {
        let mut address_ids: Vec<i64> = self
            .events
            .iter()
            .filter_map(|event| match *event {
                WalletEvent::TxDetected(ref record) | WalletEvent::TxUpdated(ref record) => {
                    Some(record.address_id)
                }
                WalletEvent::BalanceChanged(_)
                | WalletEvent::TxConfirmed(_)
                | WalletEvent::SyncProgress(_) => None,
            })
            .collect();
        address_ids.sort_unstable();
        address_ids.dedup();
        address_ids
    }
}

/// Trims `values` to the newest `capacity`, reporting whether anything was lost.
fn drop_oldest<T>(values: &mut Vec<T>, capacity: usize) -> bool {
    let over = values.len().saturating_sub(capacity);
    if over == 0 {
        return false;
    }
    values.drain(..over);
    true
}

/// Coalesced buffer for one flush window.
#[derive(Debug, Default)]
struct Flush {
    balances: BTreeMap<(i64, AssetId), Amount>,
    progress: BTreeMap<ChainId, ChainSyncProgress>,
    lossless: Vec<WalletEvent>,
    scans: Scans,
    secrets: SecretScans,
}

impl Flush {
    /// A window that starts with what the previous one couldn't persist. The carried
    /// values go in first, so anything newer overwrites them.
    fn carrying(carry: Carry) -> Self {
        let mut flush = Self::default();
        for balance in carry.balances {
            flush.balances.insert((balance.address_id, balance.asset), balance.balance);
        }
        flush.scans = carry.scans;
        flush.secrets = carry.secrets;
        for event in carry.events {
            flush.absorb(Inbound::Event(event));
        }
        flush
    }

    fn is_empty(&self) -> bool {
        self.balances.is_empty()
            && self.progress.is_empty()
            && self.lossless.is_empty()
            && self.scans.is_empty()
            && self.secrets.is_empty()
    }

    /// The coalescing kinds keep only the latest value, which for a cursor is the
    /// furthest. The lossless kinds keep arrival order and every one is delivered.
    fn absorb(&mut self, item: Inbound) {
        let event = match item {
            Inbound::Advance { advance, .. } => {
                self.scans.insert((advance.scope, advance.stream), advance.cursor);
                return;
            }
            Inbound::SecretAdvance { wallet_id, advance, .. } => {
                let secret = SecretCursor { wallet_id, cursor: advance.cursor };
                self.secrets.insert((advance.scope, advance.stream), secret);
                return;
            }
            Inbound::Event(event) => event,
        };
        match event {
            WalletEvent::BalanceChanged(update) => {
                self.balances.insert((update.address_id, update.asset), update.balance);
            }
            WalletEvent::SyncProgress(progress) => {
                self.progress.insert(progress.chain(), progress);
            }
            // Which of the two goes out is the store's answer, not the sender's.
            WalletEvent::TxDetected(record) | WalletEvent::TxUpdated(record) => {
                self.lossless.push(WalletEvent::TxDetected(record));
            }
            WalletEvent::TxConfirmed(confirmation) => {
                self.lossless.push(WalletEvent::TxConfirmed(confirmation));
            }
        }
    }

    /// The window's transaction records in arrival order, which is the order the store
    /// answers in, split at each inclusion a reorganization dropped. The records after the
    /// last split come back on their own.
    fn txs(&self) -> (Vec<DroppedInclusion>, Vec<TxRecord>) {
        let mut dropped = Vec::new();
        let mut txs = Vec::new();
        for event in &self.lossless {
            match *event {
                WalletEvent::TxDetected(ref record) | WalletEvent::TxUpdated(ref record) => {
                    txs.push(record.clone());
                }
                WalletEvent::TxConfirmed(ref confirmation)
                    if is_dropped_inclusion(confirmation) =>
                {
                    let txid = confirmation.txid.clone();
                    dropped.push(DroppedInclusion { preceding: take(&mut txs), txid });
                }
                WalletEvent::BalanceChanged(_)
                | WalletEvent::TxConfirmed(_)
                | WalletEvent::SyncProgress(_) => {}
            }
        }
        (dropped, txs)
    }

    fn balance_updates(&self) -> Vec<BalanceUpdate> {
        self.balances
            .iter()
            .map(|(&(address_id, asset), &balance)| BalanceUpdate { address_id, asset, balance })
            .collect()
    }

    /// The window's database-backed cursors. Vault-backed ones can't join the `SQLite`
    /// transaction, so they're written after it.
    fn advances(&self) -> Vec<ScanAdvance> {
        self.scans
            .iter()
            .map(|(&(scope, stream), cursor)| ScanAdvance { scope, stream, cursor: cursor.clone() })
            .collect()
    }

    /// Persists a window before publishing it, so the interface can query the announced
    /// rows. `SQLite` cursors commit with their rows, and vault cursors commit afterward.
    ///
    /// A failed write keeps the durable events for retry without publishing them.
    /// Progress is published either way.
    async fn apply(self, inner: &Inner, capacity: usize) -> Carry {
        if self.is_empty() {
            return Carry::default();
        }
        for progress in self.progress.values() {
            inner.chains.status.record_progress(*progress);
        }

        let balances = self.balance_updates();
        let (dropped, txs) = self.txs();
        let batch = SyncBatch { txs, balances: balances.clone(), scans: self.advances() };
        let Self { progress, lossless, scans, secrets, .. } = self;
        let written = if dropped.is_empty() && batch.is_empty() {
            Ok(Vec::new())
        } else {
            inner
                .store
                .transact(move |writer| {
                    let mut writes = Vec::new();
                    for DroppedInclusion { preceding, txid } in dropped {
                        let rows = SyncBatch { txs: preceding, ..SyncBatch::default() };
                        writes.extend(writer.apply_sync_batch(&rows)?);
                        writer.clear_tx_inclusion(&txid)?;
                    }
                    writes.extend(writer.apply_sync_batch(&batch)?);
                    Ok(writes)
                })
                .await
        };
        let carry = match written {
            Err(error) => {
                tracing::warn!(target: TARGET, %error, "a window of sync results was not persisted");
                // The cursors go back with their rows, so the next attempt commits both or
                // neither.
                Carry { events: lossless, balances, scans, secrets }
            }
            Ok(writes) => {
                rebroadcast(inner, lossless, balances, writes);
                Carry { secrets: write_secrets(inner, secrets).await, ..Carry::default() }
            }
        };

        for &progress in progress.values() {
            inner.events.publish(WalletEvent::SyncProgress(progress));
        }
        carry.bounded(capacity, &inner.events)
    }
}

/// An adapter reports an inclusion that a reorganization removed as zero confirmations.
const fn is_dropped_inclusion(confirmation: &TxConfirmation) -> bool {
    confirmation.confirmations == 0
}

/// An inclusion a reorganization removed, and the window's records that arrived before
/// it.
///
/// A record without a height keeps the one its row holds, so the clear has to land at
/// its point in arrival order, after the records before it and before the ones after.
#[derive(Debug)]
struct DroppedInclusion {
    preceding: Vec<TxRecord>,
    txid: TxId,
}

/// `writes` holds one answer per transaction record in `lossless`, in the same order.
fn rebroadcast(
    inner: &Inner,
    lossless: Vec<WalletEvent>,
    balances: Vec<BalanceUpdate>,
    writes: Vec<TxWrite>,
) {
    let mut writes = writes.into_iter();
    for event in lossless {
        let event = match event {
            WalletEvent::TxDetected(record) | WalletEvent::TxUpdated(record) => {
                announcement(record, writes.next())
            }
            WalletEvent::BalanceChanged(_)
            | WalletEvent::TxConfirmed(_)
            | WalletEvent::SyncProgress(_) => event,
        };
        inner.events.publish(event);
    }
    for update in balances {
        inner.events.publish(WalletEvent::BalanceChanged(update));
    }
}

/// Writes the vault cursors of a window whose rows are committed, and hands back the
/// ones worth retrying.
async fn write_secrets(inner: &Inner, secrets: SecretScans) -> SecretScans {
    let mut retries = SecretScans::new();
    for ((scope, stream), secret) in secrets {
        match write_secret(inner, secret.wallet_id, scope, stream.as_str(), &secret.cursor).await {
            Ok(()) => {}
            // The session that made this cursor is gone, so a retry could file it under the
            // wrong wallet.
            Err(CoreError::Locked) => tracing::info!(
                target: TARGET,
                stream = stream.as_str(),
                "a secret scan cursor outlived its session; that window is scanned again"
            ),
            Err(error) => {
                tracing::warn!(
                    target: TARGET,
                    %error,
                    stream = stream.as_str(),
                    "a secret scan cursor was not written and will be retried"
                );
                retries.insert((scope, stream), secret);
            }
        }
    }
    retries
}

/// Drains the sink, coalesces one window, writes it in one transaction, then
/// rebroadcasts.
///
/// Ends on cancellation or once every sender is gone. The engine holds a sender for its
/// whole life, so shutdown relies on the cancellation arm.
pub(crate) async fn run(
    weak: Weak<Inner>,
    mut receiver: mpsc::Receiver<Inbound>,
    config: EngineConfig,
    cancel: CancellationToken,
) {
    let capacity = config.channel_capacity();
    let attempts = config.attempts();
    let mut buffer: Vec<Inbound> = Vec::with_capacity(capacity);
    let mut carry = Carry::default();
    let mut failures: u32 = 0;
    loop {
        if carry.is_empty() {
            let received = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                received = receiver.recv_many(&mut buffer, capacity) => received,
            };
            if received == 0 {
                break;
            }
            // Debounce, but never outlive a cancellation. The window still flushes, and
            // the next iteration ends the task.
            tokio::select! {
                biased;
                () = cancel.cancelled() => {}
                () = sleep(config.event_flush) => {}
            }
        } else {
            // A window is waiting to be retried. New events may join it, but the retry is
            // paced so an unusable database isn't hammered.
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                () = sleep(config.retry_delay) => {}
            }
        }
        while let Ok(item) = receiver.try_recv() {
            buffer.push(item);
            if buffer.len() >= capacity {
                break;
            }
        }

        let Some(inner) = weak.upgrade() else {
            break;
        };
        let losses = inner.events.losses();
        let mut flush = Flush::carrying(take(&mut carry));
        #[expect(clippy::iter_with_drain, reason = "the buffer is reused across flush windows")]
        for item in buffer.drain(..) {
            if item.is_stale(losses) {
                tracing::debug!(
                    target: TARGET,
                    "a scan cursor read before sync results were dropped was refused; that \
                     window is scanned again"
                );
                continue;
            }
            flush.absorb(item);
        }
        carry = flush.apply(&inner, capacity).await;

        if carry.is_empty() {
            failures = 0;
        } else {
            failures = failures.saturating_add(1);
            if failures >= attempts {
                take(&mut carry).abandon(&inner, failures).await;
                failures = 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use jelly_core::{EventSink as _, TxDirection};
    use jelly_store::TxFilter;
    use tokio::sync::broadcast::{self, error::RecvError};

    use super::*;
    use crate::fixtures::{deps, harness, seed_wallet};
    use crate::test_util::{BreakableClock, config};
    use crate::{EngineDeps, WalletEngine};

    fn record(txid: &str) -> TxRecord {
        TxRecord::builder(
            AssetId::Native(ChainId::Bitcoin),
            1,
            TxId::new(ChainId::Bitcoin, txid),
            TxDirection::Incoming,
            Amount::from_base_units(1),
        )
        .build()
    }

    fn balance(units: u128) -> Inbound {
        Inbound::Event(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: 1,
            asset: AssetId::Native(ChainId::Bitcoin),
            balance: Amount::from_base_units(units),
        }))
    }

    fn detected(txid: &str) -> Inbound {
        Inbound::Event(WalletEvent::TxDetected(record(txid)))
    }

    fn advance(address_id: i64, cursor: &[u8]) -> Inbound {
        Inbound::Advance {
            losses_seen: 0,
            advance: ScanAdvance {
                scope: CursorScope::Address(address_id),
                stream: ScanStream::BitcoinElectrumStatus,
                cursor: CursorBytes::copied(cursor),
            },
        }
    }

    fn txids(records: &[TxRecord]) -> Vec<String> {
        records.iter().map(|record| record.txid.value().to_owned()).collect()
    }

    fn detected_ids(events: &[WalletEvent]) -> Vec<String> {
        events
            .iter()
            .filter_map(|event| match *event {
                WalletEvent::TxDetected(ref record) => Some(record.txid.value().to_owned()),
                WalletEvent::BalanceChanged(_)
                | WalletEvent::TxUpdated(_)
                | WalletEvent::TxConfirmed(_)
                | WalletEvent::SyncProgress(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_window_keeps_the_last_balance_and_every_transaction() {
        let mut flush = Flush::default();
        for units in 1..=3 {
            flush.absorb(balance(units));
        }
        flush.absorb(detected("one"));
        flush.absorb(detected("two"));
        flush.absorb(Inbound::Event(WalletEvent::TxConfirmed(TxConfirmation {
            txid: TxId::new(ChainId::Bitcoin, "one"),
            confirmations: 1,
        })));

        let updates = flush.balance_updates();
        assert_eq!(updates.len(), 1);
        assert_eq!(updates.first().map(|update| update.balance), Some(Amount::from_base_units(3)));
        let (dropped, txs) = flush.txs();
        assert!(dropped.is_empty(), "a confirmation that is not zero clears nothing");
        assert_eq!(txs.len(), 2);
        assert_eq!(flush.lossless.len(), 3);
    }

    /// A record without a height keeps the one its row holds, so a clear that moved past
    /// the records around it would undo a later record's inclusion or miss an earlier one.
    #[test]
    fn a_dropped_inclusion_cuts_the_window_where_it_arrived() {
        let mut flush = Flush::default();
        flush.absorb(detected("before"));
        flush.absorb(Inbound::Event(WalletEvent::TxConfirmed(TxConfirmation {
            txid: TxId::new(ChainId::Ethereum, "dropped"),
            confirmations: 0,
        })));
        flush.absorb(detected("after"));

        let (dropped, txs) = flush.txs();
        let [DroppedInclusion { preceding, txid }] = dropped.as_slice() else {
            panic!("expected one dropped inclusion, got {dropped:?}");
        };
        assert_eq!(txids(preceding), vec!["before"]);
        assert_eq!(txid.value(), "dropped");
        assert_eq!(txids(&txs), vec!["after"]);
        assert_eq!(flush.lossless.len(), 3, "the confirmation is still rebroadcast");
    }

    #[test]
    fn a_carried_window_is_overwritten_by_anything_newer() {
        let carry = Carry {
            events: vec![WalletEvent::TxDetected(record("carried"))],
            balances: vec![BalanceUpdate {
                address_id: 1,
                asset: AssetId::Native(ChainId::Bitcoin),
                balance: Amount::from_base_units(7),
            }],
            ..Carry::default()
        };
        let mut flush = Flush::carrying(carry);
        flush.absorb(balance(9));
        flush.absorb(detected("fresh"));

        assert_eq!(
            flush.balance_updates().first().map(|update| update.balance),
            Some(Amount::from_base_units(9))
        );
        let (_, txs) = flush.txs();
        assert_eq!(txids(&txs), vec!["carried", "fresh"]);
    }

    #[test]
    fn a_carry_never_grows_past_its_bound() {
        let carry = Carry {
            events: (0..10).map(|n| WalletEvent::TxDetected(record(&n.to_string()))).collect(),
            ..Carry::default()
        };
        let (events, _inbound) = Events::new(1);
        assert_eq!(detected_ids(&carry.bounded(3, &events).events), vec!["7", "8", "9"]);
    }

    #[test]
    fn a_bound_that_drops_a_row_drops_the_cursors_queued_behind_it() {
        let carry = Carry {
            events: (0..4).map(|n| WalletEvent::TxDetected(record(&n.to_string()))).collect(),
            scans: BTreeMap::from([(
                (CursorScope::Address(1), ScanStream::BitcoinElectrumStatus),
                CursorBytes::copied(b"past them"),
            )]),
            secrets: BTreeMap::from([(
                (CursorScope::Wallet, ScanStream::MoneroScan),
                SecretCursor { wallet_id: 7, cursor: CursorBytes::copied(b"past them too") },
            )]),
            ..Carry::default()
        };
        let (events, _inbound) = Events::new(1);
        let bounded = carry.bounded(2, &events);
        assert_eq!(detected_ids(&bounded.events), vec!["2", "3"]);
        assert!(bounded.scans.is_empty(), "a cursor survived the rows it was queued behind");
        assert!(bounded.secrets.is_empty());
        assert_eq!(events.losses(), 1, "the cursors still queued behind them must be refused");
    }

    #[test]
    fn a_carry_inside_its_bound_keeps_its_cursors() {
        let carry = Carry {
            events: vec![WalletEvent::TxDetected(record("one"))],
            scans: BTreeMap::from([(
                (CursorScope::Address(1), ScanStream::BitcoinElectrumStatus),
                CursorBytes::copied(b"with them"),
            )]),
            ..Carry::default()
        };
        let (events, _inbound) = Events::new(1);
        assert_eq!(carry.bounded(8, &events).scans.len(), 1);
    }

    /// One queue, so a window's cursor is the newest one emitted in it and the rows
    /// emitted before it share the window.
    #[test]
    fn a_window_keeps_one_cursor_per_stream_and_the_newest_of_them() {
        let mut flush = Flush::default();
        flush.absorb(detected("one"));
        flush.absorb(advance(1, b"first"));
        flush.absorb(advance(1, b"second"));
        flush.absorb(advance(2, b"other"));

        let advances = flush.advances();
        assert_eq!(advances.len(), 2);
        assert_eq!(flush.txs().1.len(), 1, "the rows the cursor is justified by are in the window");
        let first = advances.first().expect("an advance");
        assert_eq!(first.scope, CursorScope::Address(1));
        assert!(first.cursor.with_bytes(|bytes| bytes == b"second"));
    }

    /// A window with only a cursor is still worth a transaction, because the cursor is
    /// durable.
    #[test]
    fn a_window_with_only_a_cursor_is_worth_a_transaction() {
        let mut flush = Flush::default();
        assert!(flush.is_empty());
        flush.absorb(advance(1, b"first"));
        assert!(!flush.is_empty());
    }

    /// A vault-bound cursor never joins the batch and keeps the wallet it was produced
    /// under, because a lock and a second unlock can happen before it is written.
    #[test]
    fn a_secret_cursor_stays_out_of_the_batch_and_keeps_its_wallet() {
        let mut flush = Flush::default();
        flush.absorb(Inbound::SecretAdvance {
            wallet_id: 3,
            losses_seen: 0,
            advance: ScanAdvance {
                scope: CursorScope::Wallet,
                stream: ScanStream::MoneroScan,
                cursor: CursorBytes::copied(b"scan state"),
            },
        });
        flush.absorb(advance(1, b"electrum"));

        assert!(!flush.is_empty());
        assert_eq!(flush.advances().len(), 1, "only the plain cursor rides the batch");
        assert_eq!(
            flush
                .secrets
                .get(&(CursorScope::Wallet, ScanStream::MoneroScan))
                .map(|secret| secret.wallet_id),
            Some(3)
        );
    }

    #[test]
    fn a_carried_window_keeps_its_cursors_with_its_rows() {
        let mut scans = BTreeMap::new();
        scans.insert(
            (CursorScope::Address(1), ScanStream::BitcoinElectrumStatus),
            CursorBytes::copied(b"carried"),
        );
        let carry = Carry {
            events: vec![WalletEvent::TxDetected(record("carried"))],
            scans,
            ..Carry::default()
        };
        assert!(!carry.is_empty());
        assert_eq!(carry.address_ids(), vec![1]);

        let flush = Flush::carrying(carry);
        assert_eq!(flush.txs().1.len(), 1);
        assert_eq!(flush.advances().len(), 1);
    }

    #[test]
    fn an_empty_window_is_not_worth_a_transaction() {
        assert!(Flush::default().is_empty());
        assert!(Carry::default().is_empty());
        let mut flush = Flush::default();
        flush.absorb(balance(1));
        assert!(!flush.is_empty());
    }

    async fn attempted(
        published: &mut broadcast::Receiver<WalletEvent>,
        marker: ChainSyncProgress,
    ) {
        loop {
            match published.recv().await {
                Ok(WalletEvent::SyncProgress(progress)) if progress == marker => return,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => panic!("the engine stopped publishing"),
            }
        }
    }

    #[tokio::test]
    async fn a_cursor_queued_after_a_lossy_retry_never_commits_past_the_rows_it_lost() {
        let directory = tempfile::tempdir().unwrap();
        let clock = Arc::new(BreakableClock::default());
        let config = EngineConfig {
            event_capacity: 3,
            event_flush: Duration::from_millis(1),
            write_attempts: u32::MAX,
            retry_delay: Duration::from_millis(1),
            ..config()
        };
        let deps =
            EngineDeps { clock: Arc::<BreakableClock>::clone(&clock), ..deps(directory.path()) };
        let engine = WalletEngine::start(config, deps).await.unwrap().engine;
        let wallet = seed_wallet(&engine, "busy").await;
        let added = engine.create_imported_address(ChainId::Bitcoin, None, None).await.unwrap();
        let address_id = added.id;
        let sink = engine.inner.events.sink();
        let cursors = engine.cursors(wallet.id);
        let mut published = engine.subscribe();

        clock.set_broken(true);
        for txid in ["r0", "r1", "r2", "r3"] {
            let mut row = record(txid);
            row.address_id = address_id;
            sink.emit(WalletEvent::TxDetected(row)).await;
        }
        let refused = ChainSyncProgress::new(ChainId::Bitcoin).with_heights(None, Some(1));
        sink.emit(WalletEvent::SyncProgress(refused)).await;
        attempted(&mut published, refused).await;

        let stream = ScanStream::BitcoinElectrumStatus;
        let scope = CursorScope::Address(address_id);
        cursors.advance(scope, stream, CursorBytes::copied(b"past r3")).await.unwrap();
        clock.set_broken(false);
        let recovered = ChainSyncProgress::new(ChainId::Bitcoin).with_heights(None, Some(2));
        sink.emit(WalletEvent::SyncProgress(recovered)).await;
        attempted(&mut published, recovered).await;

        let stored = engine.inner.store.load_scans(vec![scope], vec![stream]).await.unwrap();
        let filter = TxFilter::new(wallet.id);
        let txids: Vec<String> = engine
            .inner
            .store
            .list_txs(filter)
            .await
            .unwrap()
            .rows
            .into_iter()
            .map(|row| row.txid)
            .collect();
        assert!(
            stored.is_empty() || txids.iter().any(|txid| txid == "r0"),
            "a cursor past r0 must not commit unless r0 is stored, so the next pass finds it \
             again, but the cursor was committed while the history holds only {txids:?}"
        );
    }

    /// A scope read after the loss starts from the committed position, so the pass that
    /// read it finds the lost rows again and its cursor commits.
    #[tokio::test]
    async fn a_scope_read_after_a_loss_commits_its_cursor_again() {
        let harness = harness().await;
        let engine = &harness.engine;
        let wallet = seed_wallet(engine, "reread").await;
        let added = engine.create_imported_address(ChainId::Bitcoin, None, None).await.unwrap();
        let scope = CursorScope::Address(added.id);
        let stream = ScanStream::BitcoinElectrumStatus;
        let sink = engine.inner.events.sink();
        let cursors = engine.cursors(wallet.id);
        let mut published = engine.subscribe();
        engine.inner.events.record_loss();

        cursors.advance(scope, stream, CursorBytes::copied(b"unread")).await.unwrap();
        let refused = ChainSyncProgress::new(ChainId::Bitcoin).with_heights(None, Some(1));
        sink.emit(WalletEvent::SyncProgress(refused)).await;
        attempted(&mut published, refused).await;
        let stored = engine.inner.store.load_scans(vec![scope], vec![stream]).await.unwrap();
        assert!(stored.is_empty(), "a scope not read since the loss committed its cursor");

        cursors.load_scans(&[scope], &[stream]).await.unwrap();
        cursors.advance(scope, stream, CursorBytes::copied(b"reread")).await.unwrap();
        let accepted = ChainSyncProgress::new(ChainId::Bitcoin).with_heights(None, Some(2));
        sink.emit(WalletEvent::SyncProgress(accepted)).await;
        attempted(&mut published, accepted).await;
        let stored = engine.inner.store.load_scans(vec![scope], vec![stream]).await.unwrap();
        assert!(
            stored.first().is_some_and(|row| row.cursor.with_bytes(|bytes| bytes == b"reread")),
            "a scope read since the loss must commit its cursor"
        );
    }
}
