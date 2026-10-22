//! One address's walk through the indexer history streams its [`Gate`] says are
//! due, in one pass.
//!
//! Records are emitted before each cursor advances, so persisted progress never
//! skips unreported history.

use std::collections::{HashMap, HashSet};

use jelly_core::{Deadline, EventSink, TxRecordBuilder, WalletEvent};
use serde::de::DeserializeOwned;

use crate::cursor::{Resume, Stream, StreamCursor};
use crate::error::TronError;
use crate::gate::{Explained, Gate};
use crate::history::{InternalRow, NativeRow, Page, Recorded, Row, TokenRow, place};
use crate::shape::BlockRef;
use crate::sync::PassInputs;
use crate::watched::Watched;

const TARGET: &str = "jelly::tron::walk";

/// Reads each of one address's history streams that `gate` says is due.
///
/// A stream is read only when it is due, because reading every stream of every
/// address every pass would take a wallet of a few addresses past a keyless
/// `TronGrid` budget.
///
/// A failure is logged and swallowed, because the `/v1` routes have one host,
/// `TronGrid`, which may be down, out of budget, or turned off. The cursor does not
/// move, so the next read retries and the stream is not marked complete.
pub(crate) async fn run(
    inputs: &PassInputs<'_>,
    entry: &Watched,
    tip: BlockRef,
    deadline: Deadline,
    events: &dyn EventSink,
    gate: &mut Gate,
    has_own_send: bool,
) {
    let mut walk = AddressWalk {
        inputs: *inputs,
        entry,
        tip,
        deadline,
        events,
        heights: HashMap::new(),
        reported_whole: HashSet::new(),
        unplaced_txids: HashSet::new(),
        explained: Explained::default(),
    };
    for stream in Stream::ALL {
        let cursor = inputs.states.cursor(entry.address_id, stream);
        if !gate.is_due(stream, &cursor, &walk.explained, has_own_send, inputs.clock) {
            continue;
        }
        if deadline.is_expired(inputs.clock) {
            break;
        }
        let walked = walk.stream(stream, cursor).await;
        gate.schedule_sweep(stream, inputs.clock);
        if let Err(error) = walked {
            tracing::warn!(
                target: TARGET,
                address_id = entry.address_id,
                route = stream.route().as_str(),
                %error,
                "this history stream was not walked this pass"
            );
        }
    }
    gate.reconcile(&walk.explained);
}

/// One address's walk through every history stream in one pass.
struct AddressWalk<'a> {
    inputs: PassInputs<'a>,
    entry: &'a Watched,
    tip: BlockRef,
    deadline: Deadline,
    events: &'a dyn EventSink,
    /// The block of every row this pass has read that named one, by txid. A
    /// TRC-20 row names none, and when the native stream, walked first, holds the
    /// same transaction, its block saves the receipt lookup.
    heights: HashMap<String, u64>,
    /// Every transaction whose TRX this pass has reported whole, from a native
    /// row, which lists its internal transfers, or from an internal transfer's
    /// parent row.
    reported_whole: HashSet<String>,
    /// Every transaction this pass reported with no block. A forward walk keeps
    /// its frontier below them, so a later walk reads them again and places them.
    unplaced_txids: HashSet<String>,
    explained: Explained,
}

impl AddressWalk<'_> {
    /// Forward to the frontier, then a bounded backfill, from the stored `cursor`.
    async fn stream(&mut self, stream: Stream, mut cursor: StreamCursor) -> Result<(), TronError> {
        let states = self.inputs.states;
        let address_id = self.entry.address_id;
        let before = cursor.clone();

        self.forward(stream, &mut cursor).await?;
        if !cursor.is_complete {
            self.backfill(stream, &mut cursor).await?;
        }

        // A cursor is written only when it moved, so a quiet pass costs no
        // database transaction.
        if cursor != before {
            states.set_cursor(address_id, stream, cursor);
            states.persist_scan(address_id).await?;
        }
        Ok(())
    }

    /// Walks from the newest end and stops at the transaction this wallet last
    /// reported, so a quiet address costs one page. The per-pass budget bounds a
    /// busy one, and a walk an earlier pass left short of the frontier goes on
    /// from where it stopped.
    ///
    /// The frontier never moves above a row reported with no block, so the next
    /// walk reads that row again and places it once a node has its receipt.
    async fn forward(
        &mut self,
        stream: Stream,
        cursor: &mut StreamCursor,
    ) -> Result<(), TronError> {
        let has_prior_frontier = cursor.newest.is_some();
        let (mut fingerprint, mut newest) = match cursor.resume.take() {
            Some(Resume { fingerprint, newest, newest_millis }) => {
                (Some(fingerprint), newest.map(|txid| (txid, newest_millis)))
            }
            None => (None, None),
        };
        let mut is_caught_up = false;
        let mut has_ended = false;
        for _ in 0..self.inputs.config.forward_budget() {
            let PageRead {
                row_count,
                fingerprint: next,
                newest: seen,
                has_unplaced,
                is_caught_up: is_frontier_reached,
            } = self.read_page(stream, fingerprint.as_deref(), Some(&*cursor)).await?;
            // A row with no block takes the frontier below it, wherever an
            // earlier page of this walk had put it.
            if has_unplaced || newest.is_none() {
                newest = seen;
            }
            is_caught_up = is_frontier_reached;
            fingerprint = next;
            has_ended = row_count == 0 || fingerprint.is_none();
            if is_caught_up || has_ended || self.deadline.is_expired(self.inputs.clock) {
                break;
            }
        }

        // The frontier moves only when everything above it has been reported,
        // which is when the walk reached it, the stream ended, or there was no
        // frontier and the backfill takes over where this walk stopped. A walk
        // that ran out short of the frontier records where it stopped instead, so
        // the next pass reads the rows between there and the frontier rather than
        // the same newest pages again. Duplicate records are idempotent in the
        // store, but a frontier moved over unreported history would lose it for
        // good.
        if has_prior_frontier && !is_caught_up && !has_ended {
            if let Some(fingerprint) = fingerprint {
                let (txid, timestamp_millis) = newest.unzip();
                cursor.resume = Some(Resume {
                    fingerprint,
                    newest: txid,
                    newest_millis: timestamp_millis.flatten(),
                });
            }
            return Ok(());
        }
        if let Some((txid, timestamp_millis)) = newest {
            cursor.advance_forward(&txid, timestamp_millis);
        }
        // A forward walk that reached the end of the history leaves nothing to
        // backfill. Otherwise the backfill starts where the forward walk stopped.
        if !is_caught_up && has_ended {
            cursor.finish_backfill();
        } else if !is_caught_up && cursor.backfill.is_none() {
            cursor.backfill_from(fingerprint.as_deref());
        }
        Ok(())
    }

    /// The walk into older pages, a few per pass.
    async fn backfill(
        &mut self,
        stream: Stream,
        cursor: &mut StreamCursor,
    ) -> Result<(), TronError> {
        for _ in 0..self.inputs.config.backfill_pages {
            if cursor.is_complete || self.deadline.is_expired(self.inputs.clock) {
                return Ok(());
            }
            let Some(fingerprint) = cursor.backfill.as_deref() else {
                cursor.finish_backfill();
                return Ok(());
            };
            let page = self.read_page(stream, Some(fingerprint), None).await?;
            cursor.advance_backfill(page.fingerprint.as_deref(), page.row_count);
        }
        Ok(())
    }

    /// Emits the records on one page, stopping at the frontier when `frontier`
    /// is `Some`. It is `Some` on the forward walk and `None` on the backfill.
    async fn read_page(
        &mut self,
        stream: Stream,
        fingerprint: Option<&str>,
        frontier: Option<&StreamCursor>,
    ) -> Result<PageRead, TronError> {
        let is_forward = frontier.is_some();
        match stream {
            Stream::Native => {
                let page: Page<NativeRow> = self.page(stream, fingerprint).await?;
                let fresh_rows = above_frontier(&page.data, frontier);
                for row in fresh_rows {
                    // A native row lists its internal transfers, so the TRX
                    // record it makes is already whole.
                    self.reported_whole.insert(row.txid().to_owned());
                    self.report(stream, row, is_forward).await?;
                }
                Ok(page_read(&page, fresh_rows.len(), &self.unplaced_txids))
            }
            Stream::Token => {
                let page: Page<TokenRow> = self.page(stream, fingerprint).await?;
                let fresh_rows = above_frontier(&page.data, frontier);
                for row in fresh_rows {
                    self.report(stream, row, is_forward).await?;
                }
                Ok(page_read(&page, fresh_rows.len(), &self.unplaced_txids))
            }
            Stream::Internal => {
                let page: Page<InternalRow> = self.page(stream, fingerprint).await?;
                let fresh_rows = above_frontier(&page.data, frontier);
                for row in fresh_rows {
                    self.internal(row, is_forward).await?;
                }
                Ok(page_read(&page, fresh_rows.len(), &self.unplaced_txids))
            }
        }
    }

    async fn page<T: DeserializeOwned>(
        &self,
        stream: Stream,
        fingerprint: Option<&str>,
    ) -> Result<Page<T>, TronError> {
        self.inputs.client.history(stream, &self.entry.address, fingerprint).await
    }

    /// Emits the records one row means.
    async fn report<T: Recorded + Sync>(
        &mut self,
        stream: Stream,
        row: &T,
        is_forward: bool,
    ) -> Result<(), TronError> {
        if let Some(height) = row.height() {
            self.heights.insert(row.txid().to_owned(), height);
        }
        let records = row.records(self.entry.address_id, &self.entry.address);
        if records.is_empty() {
            return Ok(());
        }
        self.emit_placed(stream, row, records, is_forward).await
    }

    /// Emits what the transaction behind an internal transfer moved for this
    /// address.
    ///
    /// The transfer is one step of that transaction and the store keeps one TRX
    /// record per transaction, so the record comes from the whole transaction,
    /// the same row read the same way as on the native stream, never from this
    /// step alone. A transaction this pass already reported whole is not read
    /// again.
    async fn internal(&mut self, row: &InternalRow, is_forward: bool) -> Result<(), TronError> {
        let txid = row.txid();
        if !row.pays(&self.entry.address) || self.reported_whole.contains(txid) {
            return Ok(());
        }
        let Some(timestamp_millis) = row.timestamp_millis() else {
            tracing::warn!(
                target: TARGET,
                address_id = self.entry.address_id,
                %txid,
                "an internal transfer with no block time cannot be matched to its transaction"
            );
            return Ok(());
        };
        let parent = self
            .inputs
            .client
            .parent(txid, timestamp_millis, self.deadline, self.inputs.clock)
            .await?;
        self.reported_whole.insert(txid.to_owned());
        let Some(record) = parent.trx_record(self.entry.address_id, &self.entry.address) else {
            tracing::warn!(
                target: TARGET,
                address_id = self.entry.address_id,
                %txid,
                "the transaction behind an internal transfer moves no TRX for this address"
            );
            return Ok(());
        };
        self.emit_placed(Stream::Internal, &parent, vec![record], is_forward).await
    }

    /// Emits the records of `row`'s transaction, each placed in its block.
    ///
    /// The store reads a record with no height as pending, so a mined row is
    /// placed before it is reported. A row no endpoint can place yet is noted in
    /// [`Self::unplaced_txids`], which keeps the forward frontier below it.
    async fn emit_placed<T: Recorded + Sync>(
        &mut self,
        stream: Stream,
        row: &T,
        records: Vec<TxRecordBuilder>,
        is_forward: bool,
    ) -> Result<(), TronError> {
        let height = match row.height() {
            Some(height) => Some(height),
            None => self.height_of(row.txid()).await?,
        };
        if height.is_none() {
            self.unplaced_txids.insert(row.txid().to_owned());
        }
        for record in records {
            let record = place(record, height, row.timestamp_millis(), self.tip.number);
            if is_forward {
                self.explained.add(stream, &record);
            }
            self.events.emit(WalletEvent::TxDetected(record)).await;
        }
        Ok(())
    }

    /// The block a row that names none was mined in, from a row this pass has
    /// already read or else from its receipt. A transaction with no receipt yet
    /// stays unplaced.
    async fn height_of(&self, txid: &str) -> Result<Option<u64>, TronError> {
        if let Some(height) = self.heights.get(txid) {
            return Ok(Some(*height));
        }
        Ok(self.inputs.client.transaction_info(txid).await?.height())
    }
}

#[derive(Debug)]
struct PageRead {
    row_count: usize,
    fingerprint: Option<String>,
    /// The row the forward frontier may move to. That is the newest on the page,
    /// or, when the page reported a transaction with no block, the newest row
    /// below the oldest such one that is not one of them. `None` when there is
    /// no such row on the page.
    newest: Option<(String, Option<i64>)>,
    /// True when the page reported a transaction with no block.
    has_unplaced: bool,
    /// True when the page reached the transaction the frontier names.
    is_caught_up: bool,
}

/// The rows above the forward frontier, which are the ones to report. With no
/// frontier every row is.
fn above_frontier<'page, T: Row>(rows: &'page [T], frontier: Option<&StreamCursor>) -> &'page [T] {
    let fresh_count = frontier
        .and_then(|cursor| {
            rows.iter().position(|row| cursor.is_caught_up(row.txid(), row.timestamp_millis()))
        })
        .unwrap_or(rows.len());
    rows.get(..fresh_count).unwrap_or_default()
}

/// What a page whose first `fresh_count` rows were reported says about its
/// stream's frontier. `unplaced_txids` is every transaction this pass reported
/// with no block.
fn page_read<T: Row>(
    page: &Page<T>,
    fresh_count: usize,
    unplaced_txids: &HashSet<String>,
) -> PageRead {
    let rows = page.data.as_slice();
    let is_unplaced = |row: &T| unplaced_txids.contains(row.txid());
    let oldest_unplaced = rows.get(..fresh_count).unwrap_or_default().iter().rposition(is_unplaced);
    let newest = match oldest_unplaced {
        None => rows.first(),
        Some(index) => rows.iter().skip(index.saturating_add(1)).find(|row| !is_unplaced(row)),
    };
    PageRead {
        row_count: rows.len(),
        fingerprint: page.fingerprint().map(ToOwned::to_owned),
        newest: newest.map(|row| (row.txid().to_owned(), row.timestamp_millis())),
        has_unplaced: oldest_unplaced.is_some(),
        is_caught_up: fresh_count < rows.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frontier_never_names_a_transaction_reported_with_no_block() {
        let page: Page<TokenRow> = serde_json::from_str(
            r#"{"data":[{"transaction_id":"aa","block_timestamp":3},
                {"transaction_id":"bb","block_timestamp":2},
                {"transaction_id":"aa","block_timestamp":2},
                {"transaction_id":"cc","block_timestamp":1}]}"#,
        )
        .expect("a page");
        let placed = page_read(&page, 4, &HashSet::new());
        assert_eq!(placed.newest, Some((String::from("aa"), Some(3))));
        assert!(!placed.has_unplaced);

        // A frontier on the second row of the same transaction would stop the
        // next walk at the first one.
        let held = page_read(&page, 4, &HashSet::from([String::from("aa")]));
        assert_eq!(held.newest, Some((String::from("cc"), Some(1))));
        assert!(held.has_unplaced);

        let oldest = page_read(&page, 4, &HashSet::from([String::from("cc")]));
        assert_eq!(oldest.newest, None, "no row on the page is below it");
        assert!(oldest.has_unplaced);
    }
}
