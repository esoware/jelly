//! One tip is read per pass and used for every confirmation depth. Records are
//! emitted before their cursors advance.

mod balances;
mod indexed;
mod logs;
mod sends;

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use jelly_core::{
    ChainId, ChainSyncProgress, Clock, CursorBytes, CursorScope, CursorStore, Deadline, EventSink,
    ScanStream, WalletEvent,
};
use parking_lot::Mutex;
use tokio::sync::Mutex as AsyncMutex;

use self::balances::{Explained, balances, read_sheets};
use self::indexed::{Indexing, indexed_history};
use self::logs::log_history;
use self::sends::poll_sends;
use crate::client::EvmClient;
use crate::config::EvmConfig;
use crate::cursor::{ScanCursor, WindowSize};
use crate::error::EvmError;
use crate::history::HistoryApi;
use crate::ledger::Ledger;
use crate::params::EvmParams;
use crate::record;
use crate::token::TokenTable;
use crate::watched::Watched;

/// The record builders run inside a pass, so they log under this target too.
pub(crate) const TARGET: &str = "jelly::evm::sync";

/// Where each address's [`ScanCursor`] lives.
const SCAN_STREAM: ScanStream = ScanStream::EvmLogScan;

pub(crate) struct PassInputs<'a> {
    pub(crate) params: &'static EvmParams,
    pub(crate) config: &'a EvmConfig,
    pub(crate) tokens: &'a TokenTable,
    pub(crate) client: &'a EvmClient,
    pub(crate) history: Option<&'a dyn HistoryApi>,
    pub(crate) cursors: &'a dyn CursorStore,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) ledger: &'a Mutex<Ledger>,
    pub(crate) writing: &'a AsyncMutex<()>,
    pub(crate) events: &'a dyn EventSink,
    /// Checked before each log window and each indexed read, so a pass that runs
    /// out keeps its progress.
    pub(crate) deadline: Deadline,
}

impl fmt::Debug for PassInputs<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PassInputs")
            .field("chain", &self.params.chain)
            .field("history", &self.history.is_some())
            .finish_non_exhaustive()
    }
}

impl PassInputs<'_> {
    fn is_out_of_time(&self) -> bool {
        self.deadline.is_expired(self.clock)
    }
}

/// What one pass leaves for the next. None of it is persisted, so a restart
/// begins again from the defaults.
#[derive(Debug)]
pub(crate) struct Carried {
    window: WindowSize,
    /// The history service said its plan doesn't cover this network, so passes
    /// read logs until the adapter is rebuilt.
    pub(crate) is_history_unserved: bool,
    indexing_by_address: HashMap<i64, Indexing>,
    /// The tip at which each address's unstored balance delta was first recorded.
    /// If a pass fails to store its judgment, the next one judges the same change
    /// again and keeps the txid the failed pass gave it.
    deltas: HashMap<i64, u64>,
}

impl Carried {
    pub(crate) fn new(params: &EvmParams, config: &EvmConfig) -> Self {
        Self {
            window: WindowSize::new(params, config),
            is_history_unserved: false,
            indexing_by_address: HashMap::new(),
            deltas: HashMap::new(),
        }
    }
}

/// Reads the tip, then receipts, then the balance sheet, then history, and judges
/// the native balances last.
///
/// Receipts and history come before the judgment so it can tell an explained
/// native change from an unexplained one, which is the only trace a native transfer
/// leaves without an indexer or native transfer logs. The sheet comes before
/// history, because an indexed pass reads only the addresses whose sheet shows news.
pub(crate) async fn run(
    inputs: &PassInputs<'_>,
    watched: &[Watched],
    carried: &mut Carried,
) -> Result<ChainSyncProgress, EvmError> {
    let chain = inputs.params.chain;
    if watched.is_empty() {
        return Ok(ChainSyncProgress::new(chain).with_progress(ChainSyncProgress::MAX_PERMILLE));
    }

    let tip = inputs.client.block_number().await?;
    let floor = inputs.params.backfill_floor;
    let ids: Vec<i64> = watched.iter().map(|entry| entry.address_id).collect();
    let mut cursors = load_cursors(inputs.cursors, &ids).await?;
    let stored = cursors.clone();
    let spends = poll_sends(inputs, watched, tip, &mut cursors).await?;
    let sheets = read_sheets(inputs, watched).await?;

    let mut explanations = BTreeMap::new();
    let mut is_indexed = false;
    if let Some(api) = inputs.history {
        let indexed = indexed_history(
            inputs,
            api,
            watched,
            &sheets,
            &mut cursors,
            &mut carried.indexing_by_address,
            tip,
        )
        .await;
        carried.is_history_unserved = indexed.is_unserved;
        is_indexed = !indexed.is_unserved;
        explanations = indexed.explanations;
    }
    if !is_indexed {
        carried.window =
            log_history(inputs, watched, &mut cursors, tip, floor, carried.window).await;
        // The log scan records every native transfer eventually, so a delta for a
        // change it hasn't reached yet would record the transfer twice.
        if inputs.params.has_native_transfer_logs {
            explanations =
                watched.iter().map(|entry| (entry.address_id, Explained::Wholly)).collect();
        }
    }

    let native = inputs.params.native();
    for change in balances(inputs, watched, &sheets, &mut cursors, &spends, &explanations) {
        let at = *carried.deltas.entry(change.entry.address_id).or_insert(tip);
        let record =
            record::of_balance_delta(&change.entry, native, change.previous, change.current, at);
        inputs.events.emit(WalletEvent::TxDetected(record)).await;
    }
    store_cursors(inputs, &stored, &cursors).await?;

    let judgment = |cursor: &ScanCursor| (cursor.native_balance, cursor.native_nonce);
    carried.deltas.retain(|address_id, _| {
        cursors
            .get(address_id)
            .is_some_and(|cursor| stored.get(address_id).map(judgment) == Some(judgment(cursor)))
    });

    Ok(progress(chain, watched, &cursors, floor, tip))
}

/// Progress is averaged over the watch set. The synced height is the lowest
/// forward frontier, and absent while any address is unscanned.
fn progress(
    chain: ChainId,
    watched: &[Watched],
    cursors: &BTreeMap<i64, ScanCursor>,
    floor: u64,
    tip: u64,
) -> ChainSyncProgress {
    let mut sum = 0u64;
    let mut lowest: Option<u64> = None;
    let mut every_address_scanned = true;
    for entry in watched {
        let cursor = cursors.get(&entry.address_id).copied().unwrap_or_default();
        sum = sum.saturating_add(u64::from(cursor.progress_permille(floor, tip)));
        match cursor.forward_to {
            Some(here) => lowest = Some(lowest.map_or(here, |low| low.min(here))),
            None => every_address_scanned = false,
        }
    }
    let counted = u64::try_from(watched.len()).unwrap_or(1).max(1);
    let permille = u16::try_from(sum.checked_div(counted).unwrap_or(0))
        .unwrap_or(ChainSyncProgress::MAX_PERMILLE);
    ChainSyncProgress::new(chain)
        .with_progress(permille)
        .with_heights(lowest.filter(|_| every_address_scanned), Some(tip))
}

pub(crate) async fn load_cursors(
    cursors: &dyn CursorStore,
    address_ids: &[i64],
) -> Result<BTreeMap<i64, ScanCursor>, EvmError> {
    let scopes: Vec<CursorScope> = address_ids.iter().copied().map(CursorScope::Address).collect();
    let rows = cursors.load_scans(&scopes, &[SCAN_STREAM]).await.map_err(EvmError::Cursor)?;
    rows.into_iter()
        .map(|row| {
            row.scope
                .address_id()
                .zip(row.cursor.with_bytes(ScanCursor::decode))
                .ok_or(EvmError::UnreadableState)
        })
        .collect()
}

async fn store_cursors(
    inputs: &PassInputs<'_>,
    stored: &BTreeMap<i64, ScanCursor>,
    cursors: &BTreeMap<i64, ScanCursor>,
) -> Result<(), EvmError> {
    for (address_id, cursor) in cursors {
        if stored.get(address_id) == Some(cursor) {
            continue;
        }
        inputs
            .cursors
            .advance(
                CursorScope::Address(*address_id),
                SCAN_STREAM,
                CursorBytes::new(cursor.encode()?),
            )
            .await
            .map_err(EvmError::Cursor)?;
    }
    Ok(())
}
