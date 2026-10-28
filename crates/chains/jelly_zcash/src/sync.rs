use std::collections::{BTreeMap, BTreeSet};

use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainSyncProgress, Clock, Deadline, EventSink, TxConfirmation,
    TxId, TxOutcome, WalletEvent,
};
use zcash_primitives::transaction::Transaction;
use zcash_transparent::bundle::{OutPoint, TxOut};

use crate::client::{Tip, TxCache, ZcashClient};
use crate::config::CHAIN;
use crate::cursor::Cursor;
use crate::entry::SPEC;
use crate::error::ZcashError;
use crate::pending;
use crate::record::{self, Record};
use crate::state::States;
use crate::watched::Watched;

// The most transactions one pass fetches for an address, so a long history is committed
// over several passes.
const MAX_FETCHES_PER_PASS: usize = 100;

/// The state one pass shares across every watched address.
struct Pass<'a> {
    client: &'a ZcashClient,
    states: &'a States,
    tip: &'a Tip,
    /// Every watched address, so a record can tell the wallet's own from strangers'.
    own: &'a [&'a str],
    /// The outputs spent by history transactions that no pass has summarized yet.
    fetched_previous_outputs: &'a mut BTreeMap<OutPoint, TxOut>,
    clock: &'a dyn Clock,
    deadline: Deadline,
    events: &'a dyn EventSink,
}

pub(crate) async fn run(
    client: &ZcashClient,
    states: &States,
    watched: &[Watched],
    fetched_previous_outputs: &mut BTreeMap<OutPoint, TxOut>,
    clock: &dyn Clock,
    deadline: Deadline,
    events: &dyn EventSink,
) -> Result<ChainSyncProgress, ZcashError> {
    states.ensure_readable()?;
    let tip = client.tip().await?;
    let progress = ChainSyncProgress::new(CHAIN).with_heights(None, Some(u64::from(tip.height)));
    let own: Vec<&str> = watched.iter().map(|entry| entry.address.as_str()).collect();
    let mut pass = Pass {
        client,
        states,
        tip: &tip,
        own: &own,
        fetched_previous_outputs,
        clock,
        deadline,
        events,
    };
    for (index, entry) in watched.iter().enumerate() {
        if !pass.sync_address(entry).await? {
            return Ok(progress);
        }
        let done = u64::try_from(index.saturating_add(1)).unwrap_or(u64::MAX);
        let total = u64::try_from(watched.len()).unwrap_or(u64::MAX);
        events.try_emit(WalletEvent::SyncProgress(progress.with_fraction(done, total)));
    }
    pass.fetched_previous_outputs.clear();
    Ok(progress
        .with_heights(Some(u64::from(tip.height)), Some(u64::from(tip.height)))
        .with_progress(ChainSyncProgress::MAX_PERMILLE))
}

impl Pass<'_> {
    /// Reads the history of `entry`, commits what it read, and reports whether that was
    /// all of it.
    async fn sync_address(&mut self, entry: &Watched) -> Result<bool, ZcashError> {
        let tip = self.tip;
        let previous = self.states.cursor(entry.address_id).await?;
        let can_reuse = previous.tip_height > 0
            && previous.tip_height <= tip.height
            && self.client.block_hash(previous.tip_height).await? == previous.tip_hash;
        let history = self.client.history(&entry.address).await?;
        let (mut records, mut unverified) = if can_reuse {
            (previous.records.clone(), previous.unverified.clone())
        } else {
            let mut unverified = previous.unverified.clone();
            unverified.extend(previous.records.clone());
            (BTreeMap::new(), unverified)
        };
        let mut cache = TxCache::new();
        let mut fetched_count: usize = 0;
        let mut is_complete = true;
        for txid in &history.txids {
            let cached = previous.records.get(txid).filter(|record| {
                can_reuse
                    && record.height.is_some_and(|height| {
                        height <= tip.height.saturating_sub(SPEC.settled_confirmations)
                    })
            });
            let record = if let Some(cached) = cached {
                cached.clone()
            } else {
                if fetched_count >= MAX_FETCHES_PER_PASS || self.deadline.is_expired(self.clock) {
                    is_complete = false;
                    break;
                }
                match self.record(txid, &entry.address, &mut cache).await {
                    Ok(Some(record)) => {
                        fetched_count = fetched_count.saturating_add(1);
                        record
                    }
                    Ok(None) => {
                        is_complete = false;
                        break;
                    }
                    // After some progress, a network failure ends the pass incomplete so the
                    // progress is saved. With none, the failure is returned, so a broken
                    // endpoint is reported instead of looking like an endless partial sync.
                    Err(ZcashError::Net(_)) if fetched_count > 0 => {
                        is_complete = false;
                        break;
                    }
                    Err(error) => return Err(error),
                }
            };
            unverified.remove(txid);
            records.insert(txid.clone(), record);
        }
        if is_complete {
            let check = self.client.history_page(&entry.address, 1).await?;
            if !check.is_same_snapshot(&history)
                || !history.txids.starts_with(&check.txids)
                || self.client.block_hash(tip.height).await? != tip.hash
            {
                return Err(ZcashError::Protocol);
            }
            let current: BTreeSet<&str> = history.txids.iter().map(String::as_str).collect();
            records.retain(|txid, _| current.contains(txid.as_str()));
            unverified.clear();
        }
        let cursor = Cursor::new(tip.height, tip.hash.clone(), records, unverified, is_complete);
        self.commit(entry, &cursor, &previous, history.balance_zat()?).await?;
        Ok(is_complete)
    }

    /// The record of `txid` for `address`, or `None` when the deadline passes before the
    /// outputs it spends are all fetched.
    async fn record(
        &mut self,
        txid: &str,
        address: &str,
        cache: &mut TxCache,
    ) -> Result<Option<Record>, ZcashError> {
        let info = self.client.tx(txid, cache).await?;
        if info.height.is_some_and(|height| height > self.tip.height) {
            return Err(ZcashError::Protocol);
        }
        let Some(previous_outputs) = self.previous_outputs(&info.tx, cache).await? else {
            return Ok(None);
        };
        record::summarize(&info, address, self.own, &previous_outputs).map(Some)
    }

    /// The outputs `tx` spends, or `None` when the deadline passes before they are all
    /// fetched.
    async fn previous_outputs(
        &mut self,
        tx: &Transaction,
        cache: &mut TxCache,
    ) -> Result<Option<BTreeMap<OutPoint, TxOut>>, ZcashError> {
        let mut outputs = BTreeMap::new();
        if let Some(bundle) = tx.transparent_bundle()
            && !bundle.is_coinbase()
        {
            for input in &bundle.vin {
                let point = input.prevout();
                let output = if let Some(output) = self.fetched_previous_outputs.get(point) {
                    output.clone()
                } else {
                    if self.deadline.is_expired(self.clock) {
                        return Ok(None);
                    }
                    let previous = self.client.tx(&point.txid().to_string(), cache).await?;
                    let output = previous
                        .tx
                        .transparent_bundle()
                        .and_then(|bundle| bundle.vout.get(usize::try_from(point.n()).ok()?))
                        .ok_or(ZcashError::Protocol)?
                        .clone();
                    self.fetched_previous_outputs.insert(point.clone(), output.clone());
                    output
                };
                if outputs.insert(point.clone(), output).is_some() {
                    return Err(ZcashError::Protocol);
                }
            }
        }
        self.fetched_previous_outputs.retain(|point, _| !outputs.contains_key(point));
        Ok(Some(outputs))
    }

    /// Reports what `cursor` holds of `entry`'s history against `previous`, settles its
    /// sends, and then stores the cursor.
    async fn commit(
        &self,
        entry: &Watched,
        cursor: &Cursor,
        previous: &Cursor,
        balance_zat: u64,
    ) -> Result<(), ZcashError> {
        let events = self.events;
        for (txid, old) in previous.records.iter().chain(&previous.unverified) {
            if !cursor.records.contains_key(txid) && !cursor.unverified.contains_key(txid) {
                events
                    .emit(WalletEvent::TxConfirmed(TxConfirmation {
                        txid: TxId::new(CHAIN, txid),
                        confirmations: 0,
                    }))
                    .await;
                let mut orphan = old.clone();
                orphan.height = None;
                events
                    .emit(WalletEvent::TxDetected(orphan.tx_record(
                        entry.address_id,
                        cursor.tip_height,
                        TxOutcome::Unknown,
                    )))
                    .await;
            }
        }
        for record in cursor.records.values() {
            events
                .emit(WalletEvent::TxDetected(record.tx_record(
                    entry.address_id,
                    cursor.tip_height,
                    TxOutcome::Success,
                )))
                .await;
        }
        pending::follow(self.states, entry, cursor, events).await?;
        events.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: entry.address_id,
            asset: AssetId::Native(CHAIN),
            balance: Amount::from(balance_zat),
        }));
        self.states.advance(entry.address_id, cursor).await
    }
}
