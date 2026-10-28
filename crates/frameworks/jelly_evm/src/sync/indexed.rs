use std::collections::{BTreeMap, HashMap};

use alloy::primitives::B256;
use jelly_core::{AssetId, Clock, Deadline, TxRecord, WalletEvent};

use super::balances::{Explained, Sheet};
use super::{PassInputs, TARGET};
use crate::cursor::{Frontier, ScanCursor, Window};
use crate::error::{EvmError, HistoryRefusal};
use crate::history::{HistoryApi, HistoryPage, HistoryRow, InternalRow, NativeRow, TokenRow};
use crate::record;
use crate::watched::Watched;

/// What decides when the indexer next reads one address.
///
/// An address is due while it has news no read has reported, once its sweep comes
/// round, and on the first pass that sees it, which after a restart is every
/// address.
#[derive(Debug, Default)]
pub(super) struct Indexing {
    /// The sheet the last pass read, which the next pass compares its own against.
    sheet: Sheet,
    /// Unset until a read completes, and again while the history is truncated.
    sweep_at: Option<Deadline>,
    /// The tip at which the sheet last showed a change that no read has reported
    /// yet.
    news_at: Option<u64>,
}

impl Indexing {
    fn is_due(&self, clock: &dyn Clock) -> bool {
        self.news_at.is_some() || self.sweep_at.is_none_or(|at| at.is_expired(clock))
    }
}

/// What the indexed step leaves for the balance step, and whether the service
/// turned this network away.
#[derive(Debug, Default)]
pub(super) struct Indexed {
    pub(super) explanations: BTreeMap<i64, Explained>,
    pub(super) is_unserved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexedReadState {
    /// Reads continue.
    Active,
    /// Later addresses are deferred.
    StoppedDeferring,
    /// Later addresses are judged on what is known.
    StoppedJudging,
}

/// Reads the three lists of each address that is due, from one shared frontier.
///
/// The service is per address, so this is the one step whose cost grows with the
/// wallet. An address is due when [`Indexing::is_due`] says so, and its news ends
/// once a read returns rows past the frontier it started from, or claims the
/// blocks up to the tip that first showed the news.
///
/// An address whose read emitted a native record is explained wholly, because a
/// token transfer moves no native balance. One whose news is still unreported, or
/// whose read was truncated, or skipped for the deadline or a busy or unreachable
/// service, is deferred, because the service may still report the transfer behind
/// its change. The first failure ends the step, because the next address would ask
/// the same service.
pub(super) async fn indexed_history(
    inputs: &PassInputs<'_>,
    api: &dyn HistoryApi,
    watched: &[Watched],
    sheets: &[Sheet],
    cursors: &mut BTreeMap<i64, ScanCursor>,
    indexing_by_address: &mut HashMap<i64, Indexing>,
    tip: u64,
) -> Indexed {
    let floor = inputs.params.backfill_floor;
    let indexed = tip.saturating_sub(inputs.params.indexer_lag_blocks());
    let overlap = inputs.params.rescan_overlap_blocks();
    indexing_by_address.retain(|id, _| watched.iter().any(|entry| entry.address_id == *id));
    let mut outcome = Indexed::default();
    let mut read_state = IndexedReadState::Active;
    for (entry, sheet) in watched.iter().zip(sheets) {
        let address_id = entry.address_id;
        let mut cursor = cursors.get(&address_id).copied().unwrap_or_default();
        let has_news = match indexing_by_address.get(&address_id) {
            Some(last) => last.sheet != *sheet,
            // Across a restart only the judged native balance is remembered.
            None => cursor
                .native_balance
                .zip(sheet.native)
                .is_some_and(|(judged, balance)| balance != judged),
        };
        let state = indexing_by_address.entry(address_id).or_default();
        state.sheet = sheet.clone();
        state.news_at = has_news.then_some(tip).or(state.news_at);
        if !state.is_due(inputs.clock) {
            continue;
        }
        match read_state {
            IndexedReadState::Active if !inputs.is_out_of_time() => {}
            IndexedReadState::Active | IndexedReadState::StoppedDeferring => {
                read_state = IndexedReadState::StoppedDeferring;
                outcome.explanations.insert(address_id, Explained::Deferred);
                continue;
            }
            IndexedReadState::StoppedJudging => continue,
        }

        let start = cursor.history_from(floor, overlap);
        let read = async {
            let native = api.native(entry.owner, start).await?;
            let internal = api.internal(entry.owner, start).await?;
            let tokens = api.tokens(entry.owner, start).await?;
            let records = records_of(inputs, entry, &native, &internal, &tokens, tip).await?;
            Ok::<_, EvmError>((native, internal, tokens, records))
        };
        let (native, internal, tokens, records) = match read.await {
            Ok(pages) => pages,
            Err(EvmError::HistoryRefused(HistoryRefusal::ChainNotCovered)) => {
                tracing::info!(
                    target: TARGET,
                    "the history service does not serve this network for this key, so history comes from logs and balances"
                );
                outcome.is_unserved = true;
                return outcome;
            }
            Err(error) => {
                tracing::warn!(
                    target: TARGET,
                    address_id,
                    %error,
                    "the indexed history read failed; the balances still stand"
                );
                read_state = if error.is_transient() {
                    outcome.explanations.insert(address_id, Explained::Deferred);
                    IndexedReadState::StoppedDeferring
                } else {
                    IndexedReadState::StoppedJudging
                };
                continue;
            }
        };

        let has_native = records.iter().any(|record| matches!(record.asset, AssetId::Native(_)));
        for record in records {
            inputs.events.emit(WalletEvent::TxDetected(record)).await;
        }

        // The read covers only as far as the shortest of the three lists.
        let before = cursor.forward_to;
        let covered = covered_to(&native, start, indexed)
            .min(covered_to(&internal, start, indexed))
            .min(covered_to(&tokens, start, indexed));
        cursor.apply(Window { from: start, to: covered, frontier: Frontier::Forward });
        cursors.insert(address_id, cursor);

        let is_truncated =
            native.is_truncated() || internal.is_truncated() || tokens.is_truncated();
        if is_truncated {
            state.sweep_at = None;
        } else {
            state.sweep_at = Some(inputs.clock.deadline_in(inputs.config.history_sweep));
            let highest =
                native.highest_block().max(internal.highest_block()).max(tokens.highest_block());
            let has_new_rows = highest.is_some_and(|block| before.is_none_or(|done| block > done));
            if has_new_rows || state.news_at.is_some_and(|at| covered >= at) {
                state.news_at = None;
            }
        }
        if has_native {
            outcome.explanations.insert(address_id, Explained::Wholly);
        } else if is_truncated || state.news_at.is_some() {
            outcome.explanations.insert(address_id, Explained::Deferred);
        }
    }
    outcome
}

/// Receipts asked for in one batch, so a long history is not one request a public
/// node refuses.
const RECEIPT_BATCH: usize = 20;

/// One address's read as records.
///
/// The OP Stack charges for posting a transaction's data to Ethereum apart from gas,
/// and the service's rows price only the gas. There each transaction the owner sent
/// is priced by its receipt, as the send's own record is, so the two agree and the
/// stored fee stands. One whose receipt states no fee keeps its row's gas.
async fn records_of(
    inputs: &PassInputs<'_>,
    entry: &Watched,
    native: &HistoryPage<NativeRow>,
    internal: &HistoryPage<InternalRow>,
    tokens: &HistoryPage<TokenRow>,
    tip: u64,
) -> Result<Vec<TxRecord>, EvmError> {
    let mut receipt_fee_wei = HashMap::new();
    if inputs.params.has_l1_data_fee {
        let mut sent: Vec<B256> = native
            .rows()
            .iter()
            .filter(|row| row.from == entry.owner)
            .map(|row| row.hash)
            .collect();
        sent.sort_unstable();
        sent.dedup();
        for batch in sent.chunks(RECEIPT_BATCH) {
            let receipts = inputs.client.receipts(batch).await?;
            for (txid, receipt) in batch.iter().zip(receipts) {
                if let Some(fee) = receipt.and_then(|receipt| receipt.fee_wei()) {
                    receipt_fee_wei.insert(*txid, fee);
                }
            }
        }
    }
    let (native, internal, tokens) = (native.rows(), internal.rows(), tokens.rows());
    let coin = inputs.params.native();
    let mut records = record::of_native(coin, entry, native, internal, &receipt_fee_wei, tip);
    records.extend(record::of_tokens(inputs.tokens, entry, tokens, native, &receipt_fee_wei, tip));
    Ok(records)
}

/// The service answers oldest first.
///
/// A complete read covers what the service has indexed, which is assumed to reach
/// `indexed` and is known to reach its highest row. A truncated one stops a block
/// below its highest row, because that block may be only partly returned and a
/// duplicate row is cheaper than a hole.
fn covered_to<T: HistoryRow>(page: &HistoryPage<T>, start: u64, indexed: u64) -> u64 {
    let highest = page.highest_block();
    if page.is_truncated() {
        return highest.map_or(start, |block| block.saturating_sub(1).max(start));
    }
    highest.map_or(indexed, |block| block.max(indexed)).max(start)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::address;

    #[test]
    fn a_complete_indexed_read_claims_only_what_the_service_has_surely_indexed() {
        let empty = HistoryPage::<NativeRow>::of(Vec::new(), false);
        assert_eq!(covered_to(&empty, 10, 90), 90, "short of the tip by the lag allowance");
        assert_eq!(covered_to(&empty, 95, 90), 95, "never behind where the read started");

        let row = NativeRow {
            block_number: 97,
            timestamp: 0,
            hash: B256::ZERO,
            from: address(1),
            to: None,
            value: 0,
            gas_used: Some(0),
            gas_price: Some(0),
            is_failed: false,
        };
        let reaching = HistoryPage::of(vec![row.clone()], false);
        assert_eq!(covered_to(&reaching, 10, 90), 97, "a row proves the service got that far");

        let truncated = HistoryPage::of(vec![row], true);
        assert_eq!(covered_to(&truncated, 10, 90), 96, "a truncated read stops below its last row");
    }
}
