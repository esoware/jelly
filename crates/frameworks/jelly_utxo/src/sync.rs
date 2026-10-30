use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::mem;

use bitcoin::{OutPoint, ScriptBuf, Transaction, Txid, block};
use jelly_core::{Amount, AssetId, ChainSyncProgress, EventSink, TxOutcome, TxRecord, WalletEvent};
use jelly_electrum::{ElectrumClient, HistoryDigest, HistoryEntry, Unspent};

use crate::cursor::{CursorPending, CursorUtxo, SyncCursor};
use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::prevout::PrevoutMap;
use crate::record::{Confirmed, Viewpoint, tx_record};
use crate::script::SpendableScript;
use crate::unspent::{Funding, RefusedUnspent, verified_unspents};
use crate::utxo::Utxo;

/// One address a pass watches. The pass works in scripts and reports by
/// `address_id`, so it doesn't need the address string.
#[derive(Debug, Clone)]
pub(crate) struct WatchedScript<'a> {
    pub(crate) address_id: i64,
    pub(crate) script: &'a SpendableScript,
    /// What the last successful pass left behind, when there was one.
    pub(crate) cursor: Option<SyncCursor>,
    /// This address's mempool transactions that an earlier pass this session
    /// decoded.
    pub(crate) pending: &'a [PendingTx],
}

impl WatchedScript<'_> {
    /// The coins an earlier pass verified against their funding transactions.
    fn verified(&self) -> &[CursorUtxo] {
        self.cursor.as_ref().map_or(&[][..], SyncCursor::coins)
    }
}

/// A mempool transaction in one address's history, kept between passes.
///
/// A replacement can then outbid the descendants it would evict, and a later pass can settle
/// one that a conflicting transaction replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingTx {
    pub(crate) txid: Txid,
    pub(crate) spends: Vec<OutPoint>,
    /// As the pass that decoded it reported it.
    pub(crate) record: TxRecord,
}

/// What the cursor keeps of a pending transaction, so a later session can settle it
/// too.
impl From<&PendingTx> for CursorPending {
    fn from(tx: &PendingTx) -> Self {
        Self { txid: tx.txid, direction: tx.record.direction, spends: tx.spends.clone() }
    }
}

impl PendingTx {
    /// The record it settles into once mined. A transaction can't change, so what
    /// this session decoded is reported again at its height.
    fn confirmed(&self, at: Confirmed, tip_height: u32) -> TxRecord {
        let record = &self.record;
        let mut builder = TxRecord::builder(
            record.asset,
            record.address_id,
            record.txid.clone(),
            record.direction,
            record.amount,
        )
        .outcome(record.outcome)
        .detection(record.detection)
        .confirmed_at(u64::from(at.height), at.unix_seconds, u64::from(tip_height));
        if let Some(fee) = record.fee {
            builder = builder.fee(fee);
        }
        if let Some(counterparty) = record.counterparty.clone() {
            builder = builder.counterparty(counterparty);
        }
        builder.build()
    }
}

/// What a pass does with one entry of a history that moved.
#[derive(Debug, Clone, Copy)]
enum Step<'a> {
    /// Download and classify it.
    Decode,
    /// A mempool transaction this session decoded has been mined, so it's reported
    /// again at its height without downloading it.
    Confirm(&'a PendingTx),
    /// Still in the mempool, as this session decoded it.
    Pending(&'a PendingTx),
    /// The last pass reported it at this height.
    Unchanged,
}

/// A mempool entry this session hasn't decoded is downloaded even when the cursor
/// saw it, because a replacement has to know every pending transaction it would
/// evict.
fn step<'a>(entry: &WatchedScript<'a>, item: &HistoryEntry) -> Step<'a> {
    let is_committed = entry
        .cursor
        .as_ref()
        .is_some_and(|cursor| cursor.pending().iter().any(|pending| pending.txid == item.txid));
    let decoded = entry.pending.iter().find(|tx| tx.txid == item.txid && is_committed);
    let Some(height) = item.confirmation.height() else {
        return decoded.map_or(Step::Decode, Step::Pending);
    };
    if entry.cursor.as_ref().is_some_and(|cursor| cursor.is_seen_at(item.txid, height)) {
        return Step::Unchanged;
    }
    decoded.map_or(Step::Decode, Step::Confirm)
}

#[derive(Debug, Clone)]
pub(crate) struct AddressPass {
    pub(crate) address_id: i64,
    pub(crate) coins: Vec<Utxo>,
    pub(crate) records: Vec<TxRecord>,
    pub(crate) cursor: SyncCursor,
    /// The history hadn't moved, so nothing was refetched or reclassified.
    pub(crate) is_unchanged: bool,
    /// Unspent entries the server's own transactions didn't bear out. They're never
    /// spent, and kept here so the adapter can log them.
    pub(crate) refused: Vec<RefusedUnspent>,
    /// The history as this pass read it. Confirmation depths come from it whether
    /// or not it moved.
    pub(crate) history: Vec<HistoryEntry>,
    /// Its mempool transactions, decoded by this pass or, when the history hasn't
    /// moved, carried in from the last one.
    pub(crate) pending: Vec<PendingTx>,
}

#[derive(Debug, Clone)]
pub(crate) struct PassOutcome {
    pub(crate) tip_height: u32,
    pub(crate) addresses: Vec<AddressPass>,
}

impl PassOutcome {
    /// The terminal progress, which the adapter hands back for the engine to
    /// publish. Adapters emit only intermediate progress.
    pub(crate) fn progress(&self, params: &UtxoParams) -> ChainSyncProgress {
        let tip = u64::from(self.tip_height);
        ChainSyncProgress::new(params.chain)
            .with_progress(ChainSyncProgress::MAX_PERMILLE)
            .with_heights(Some(tip), Some(tip))
    }

    /// Every outpoint the pass saw unspent, for reconciling outpoint claims.
    pub(crate) fn live_outpoints(&self) -> Vec<OutPoint> {
        self.addresses.iter().flat_map(|address| address.coins.iter().map(Utxo::outpoint)).collect()
    }
}

/// Runs one sync pass over `watched` against one server.
///
/// The outcome is a pure function of what this pass fetched and what `watched`
/// carried in, so a pass that fails midway leaves every stored cursor intact,
/// and a reorg needs no special handling.
pub(crate) async fn sync_pass(
    params: &UtxoParams,
    client: &ElectrumClient,
    watched: &[WatchedScript<'_>],
    events: &dyn EventSink,
) -> Result<PassOutcome, UtxoError> {
    let view = ServerView::fetch(client, watched).await?;
    reconcile(params, watched, view, events)
}

struct ServerView {
    tip_height: u32,
    histories: Vec<Vec<HistoryEntry>>,
    /// The unspent sets of the addresses whose history moved, by watch index. An
    /// address counts as changed exactly when it has an entry here.
    unspents: BTreeMap<usize, Vec<Unspent>>,
    txs: BTreeMap<Txid, Transaction>,
    /// The members of `txs` served as a Litecoin `HogEx`.
    hogexes: BTreeSet<Txid>,
    prevouts: PrevoutMap,
    headers: BTreeMap<u32, block::Header>,
}

impl ServerView {
    /// Fetches the tip and every history, then, for the addresses whose history
    /// moved, the unspent sets, the transactions not yet reported at their current
    /// height, the parents needed to price those, and the headers that date them.
    ///
    /// A transaction never changes once it has a txid, so one the cursor saw at the
    /// same height isn't downloaded again. An empty batch sends no request, so an
    /// empty watch set costs one round trip.
    async fn fetch(
        client: &ElectrumClient,
        watched: &[WatchedScript<'_>],
    ) -> Result<Self, UtxoError> {
        let tip_height = client.tip_height().await?;
        let scripts: Vec<ScriptBuf> =
            watched.iter().map(|entry| entry.script.script_buf().clone()).collect();
        let histories = client.histories(&scripts).await?;

        // A history with a mempool transaction this session hasn't decoded is
        // refetched even when its cursor matches, so a replacement knows every
        // pending transaction it would evict.
        let changed: Vec<usize> = watched
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                let history = history_at(&histories, *index);
                let is_current = entry
                    .cursor
                    .as_ref()
                    .is_some_and(|cursor| cursor.matches(HistoryDigest::of(history)));
                !is_current
                    || history.iter().any(|item| {
                        !item.confirmation.is_confirmed()
                            && !entry.pending.iter().any(|tx| tx.txid == item.txid)
                    })
            })
            .map(|(index, _)| index)
            .collect();

        // The unspent sets are read before the transactions, so a coin funded after
        // the history read is still verified in this pass.
        let changed_scripts: Vec<ScriptBuf> = changed
            .iter()
            .filter_map(|index| watched.get(*index))
            .map(|entry| entry.script.script_buf().clone())
            .collect();
        let listed = client.unspents(&changed_scripts).await?;
        let unspents: BTreeMap<usize, Vec<Unspent>> = changed.into_iter().zip(listed).collect();

        let wanted = wanted(watched, &histories, &unspents);
        let served = client.txs(&wanted).await?;
        let hogexes: BTreeSet<Txid> = served
            .iter()
            .filter(|served| served.is_hogex)
            .map(|served| served.tx.compute_txid())
            .collect();
        let txs: BTreeMap<Txid, Transaction> =
            served.into_iter().map(|served| (served.tx.compute_txid(), served.tx)).collect();

        let mut prevouts = PrevoutMap::index(txs.values());
        let parents: Vec<Txid> = prevouts.missing_parents(txs.values()).into_iter().collect();
        for served in client.txs(&parents).await? {
            prevouts.add(&served.tx);
        }

        let heights = reported_heights(watched, &histories, &unspents);
        let headers: BTreeMap<u32, block::Header> =
            heights.iter().copied().zip(client.block_headers(&heights).await?).collect();

        Ok(Self { tip_height, histories, unspents, txs, hogexes, prevouts, headers })
    }
}

/// Pure apart from the progress it emits.
fn reconcile(
    params: &UtxoParams,
    watched: &[WatchedScript<'_>],
    mut view: ServerView,
    events: &dyn EventSink,
) -> Result<PassOutcome, UtxoError> {
    let total = watched.len();
    let mut addresses = Vec::with_capacity(total);
    let replaced = replaced_pending(watched, &view.histories, &view.txs);
    let heights = heights_of(&view.histories);
    let mut histories = mem::take(&mut view.histories).into_iter();
    let funding = Funding {
        params,
        txs: &view.txs,
        hogexes: &view.hogexes,
        prevouts: &view.prevouts,
        own_scripts: watched.iter().map(|entry| entry.script.script_pubkey()).collect(),
    };
    let read = Read { view: &view, funding: &funding, replaced: &replaced, heights: &heights };
    for (index, entry) in watched.iter().enumerate() {
        emit_progress(params, events, index, total, view.tip_height);
        let history = histories.next().unwrap_or_default();
        // An address without a cursor always counts as changed, so only one with a
        // cursor is ever restored.
        let address = match (view.unspents.get(&index), entry.cursor.as_ref()) {
            (None, Some(cursor)) => restored(entry, cursor, history),
            (listed, _) => {
                refreshed(params, entry, history, listed.map_or(&[], Vec::as_slice), &read)?
            }
        };
        addresses.push(address);
    }

    Ok(PassOutcome { tip_height: view.tip_height, addresses })
}

/// An address whose history hasn't moved, carried over from its cursor with
/// nothing refetched or reclassified.
fn restored(
    entry: &WatchedScript<'_>,
    cursor: &SyncCursor,
    history: Vec<HistoryEntry>,
) -> AddressPass {
    // A pass cut short after advancing the cursor can leave behind a pending
    // transaction the history no longer holds.
    let pending = entry
        .pending
        .iter()
        .filter(|tx| {
            history.iter().any(|item| item.txid == tx.txid && !item.confirmation.is_confirmed())
        })
        .cloned()
        .collect();
    AddressPass {
        address_id: entry.address_id,
        coins: cursor.restore(entry.script),
        records: Vec::new(),
        cursor: cursor.clone(),
        is_unchanged: true,
        refused: Vec::new(),
        history,
        pending,
    }
}

/// The confirming height of every transaction any watched history names, absent while
/// it's in the mempool. A transaction that any one history shows mined counts as
/// mined.
fn heights_of(histories: &[Vec<HistoryEntry>]) -> HashMap<Txid, Option<u32>> {
    let mut heights: HashMap<Txid, Option<u32>> = HashMap::new();
    for item in histories.iter().flatten() {
        let height = item.confirmation.height();
        let known = heights.entry(item.txid).or_insert(height);
        if known.is_none() {
            *known = height;
        }
    }
    heights
}

/// What one pass read that every changed address is classified against.
struct Read<'a> {
    view: &'a ServerView,
    funding: &'a Funding<'a>,
    /// Pending transactions a replacement displaced.
    replaced: &'a BTreeSet<Txid>,
    heights: &'a HashMap<Txid, Option<u32>>,
}

/// An address whose history moved. Its unspent set is verified, every history entry
/// not yet reported at its current height is classified, and whatever it left
/// pending that's in `read.replaced` is settled.
fn refreshed(
    params: &UtxoParams,
    entry: &WatchedScript<'_>,
    history: Vec<HistoryEntry>,
    listed: &[Unspent],
    read: &Read<'_>,
) -> Result<AddressPass, UtxoError> {
    let Read { view, funding, replaced, heights } = *read;
    let (coins, refused) = verified_unspents(listed, entry.script, entry.verified(), funding);

    let viewpoint = Viewpoint {
        address_id: entry.address_id,
        script_pubkey: entry.script.script_pubkey(),
        own_scripts: &funding.own_scripts,
        heights,
    };
    let mut records = Vec::new();
    let mut pending = Vec::new();
    for item in &history {
        let confirmed = item.confirmation.height().map(|height| Confirmed {
            height,
            unix_seconds: view.headers.get(&height).map(|header| i64::from(header.time)),
        });
        match step(entry, item) {
            Step::Unchanged => {}
            Step::Pending(tx) => pending.push(tx.clone()),
            Step::Confirm(tx) => {
                if let Some(at) = confirmed {
                    records.push(tx.confirmed(at, view.tip_height));
                }
            }
            Step::Decode => {
                let Some(tx) = view.txs.get(&item.txid) else { continue };
                let record =
                    tx_record(params, &viewpoint, tx, &view.prevouts, confirmed, view.tip_height)?;
                if confirmed.is_none() {
                    pending.push(PendingTx {
                        txid: item.txid,
                        spends: tx.input.iter().map(|txin| txin.previous_output).collect(),
                        record: record.clone(),
                    });
                }
                records.push(record);
            }
        }
    }
    for left in left_pending(entry) {
        if replaced.contains(&left.txid) {
            records.push(settled(params, entry.address_id, &left));
        }
    }

    let remembered = pending.iter().map(CursorPending::from).collect();
    let cursor = SyncCursor::new(HistoryDigest::of(&history), &coins, &history, remembered);
    Ok(AddressPass {
        address_id: entry.address_id,
        coins,
        records,
        cursor,
        is_unchanged: false,
        refused,
        history,
        pending,
    })
}

/// What the last pass to report `entry` left pending, from both this session's
/// decoded transactions and its cursor.
///
/// After a restart only the cursor has them, and a pass cut short after advancing
/// the cursor leaves it ahead of this session.
fn left_pending(entry: &WatchedScript<'_>) -> Vec<CursorPending> {
    let mut left: BTreeMap<Txid, CursorPending> =
        entry.cursor.iter().flat_map(SyncCursor::pending).map(|tx| (tx.txid, tx.clone())).collect();
    for tx in entry.pending {
        left.insert(tx.txid, CursorPending::from(tx));
    }
    left.into_values().collect()
}

/// The transactions an earlier pass left pending that can never confirm.
///
/// That's each one that conflicts with a transaction this pass downloaded, and each
/// one that spends an output of a replaced one, at whichever watched address it was
/// left. Only a transaction gone from every history counts, since a server that still
/// lists one hasn't seen it replaced.
fn replaced_pending(
    watched: &[WatchedScript<'_>],
    histories: &[Vec<HistoryEntry>],
    txs: &BTreeMap<Txid, Transaction>,
) -> BTreeSet<Txid> {
    let listed: BTreeSet<Txid> = histories.iter().flatten().map(|item| item.txid).collect();
    let gone: BTreeMap<Txid, Vec<OutPoint>> = watched
        .iter()
        .flat_map(left_pending)
        .filter(|tx| !listed.contains(&tx.txid))
        .map(|tx| (tx.txid, tx.spends))
        .collect();
    let spent: BTreeSet<OutPoint> = txs
        .iter()
        .filter(|(txid, _)| !gone.contains_key(*txid))
        .flat_map(|(_, tx)| tx.input.iter().map(|txin| txin.previous_output))
        .collect();
    let mut replaced: BTreeSet<Txid> = gone
        .iter()
        .filter(|(_, spends)| spends.iter().any(|outpoint| spent.contains(outpoint)))
        .map(|(txid, _)| *txid)
        .collect();
    let mut has_grown = true;
    while has_grown {
        has_grown = false;
        for (txid, spends) in &gone {
            if !replaced.contains(txid)
                && spends.iter().any(|outpoint| replaced.contains(&outpoint.txid))
            {
                replaced.insert(*txid);
                has_grown = true;
            }
        }
    }
    replaced
}

/// The record that settles a pending transaction a conflicting one replaced. It
/// never confirmed, so it moved nothing and cost nothing.
///
/// A cursor keeps no counterparty, so the record names none and the row keeps the
/// one it was recorded with.
fn settled(params: &UtxoParams, address_id: i64, tx: &CursorPending) -> TxRecord {
    TxRecord::builder(
        AssetId::Native(params.chain),
        address_id,
        params.txid(tx.txid),
        tx.direction,
        Amount::ZERO,
    )
    .outcome(TxOutcome::Replaced)
    .fee(Amount::ZERO)
    .build()
}

/// Every transaction a pass has to download for the addresses in `unspents`: the
/// history entries it has to classify, and the funding transaction of every
/// listed coin no earlier pass verified.
fn wanted(
    watched: &[WatchedScript<'_>],
    histories: &[Vec<HistoryEntry>],
    unspents: &BTreeMap<usize, Vec<Unspent>>,
) -> Vec<Txid> {
    let mut wanted: BTreeSet<Txid> = BTreeSet::new();
    for (&index, listed) in unspents {
        let Some(entry) = watched.get(index) else { continue };
        for item in history_at(histories, index) {
            if matches!(step(entry, item), Step::Decode) {
                wanted.insert(item.txid);
            }
        }
        let verified = entry.verified();
        for unspent in listed {
            if !verified.iter().any(|coin| coin.outpoint == unspent.outpoint) {
                wanted.insert(unspent.outpoint.txid);
            }
        }
    }
    wanted.into_iter().collect()
}

/// The heights whose headers date the records a pass reports.
fn reported_heights(
    watched: &[WatchedScript<'_>],
    histories: &[Vec<HistoryEntry>],
    unspents: &BTreeMap<usize, Vec<Unspent>>,
) -> Vec<u32> {
    let mut heights: BTreeSet<u32> = BTreeSet::new();
    for &index in unspents.keys() {
        let Some(entry) = watched.get(index) else { continue };
        for item in history_at(histories, index) {
            match step(entry, item) {
                Step::Decode | Step::Confirm(_) => heights.extend(item.confirmation.height()),
                Step::Pending(_) | Step::Unchanged => {}
            }
        }
    }
    heights.into_iter().collect()
}

fn history_at(histories: &[Vec<HistoryEntry>], index: usize) -> &[HistoryEntry] {
    histories.get(index).map_or(&[], Vec::as_slice)
}

/// `done < total` keeps this below terminal progress, which only the engine
/// publishes.
fn emit_progress(
    params: &UtxoParams,
    events: &dyn EventSink,
    done: usize,
    total: usize,
    tip_height: u32,
) {
    let progress = ChainSyncProgress::new(params.chain)
        .with_fraction(
            u64::try_from(done).unwrap_or(u64::MAX),
            u64::try_from(total).unwrap_or(u64::MAX),
        )
        .with_heights(None, Some(u64::from(tip_height)));
    events.try_emit(WalletEvent::SyncProgress(progress));
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use bitcoin::consensus::serialize;
    use jelly_core::{ScriptKind, TxDirection, TxId, confirmations, test_util::FakeEventSink};
    use jelly_electrum::{Confirmation, ElectrumError, Method, test_util::Answer};

    use super::*;
    use crate::fixtures::{FUNDED_SAT, Fallible, MINED_AT, RECEIVED_SAT, Received, TIP, received};
    use crate::test_util::{BITCOIN, FakeChain, TestKey, test_key, tx_paying, tx_spending};
    use crate::utxo::units;

    const BLOCK_UNIX_SECONDS: u32 = 1_700_000_000;
    const SENT_SAT: u64 = 50_000;
    const CHANGE_SAT: u64 = 120_000;
    const RETURNED_SAT: u64 = 110_000;

    struct Chain {
        ours: TestKey,
        theirs: TestKey,
        funding: Transaction,
        payment: Transaction,
        fake: FakeChain,
    }

    /// The payment [`received`] makes, mined and served by a fake server.
    fn chain() -> Fallible<Chain> {
        let Received { ours, theirs, funding, payment } = received()?;
        let fake = FakeChain::new();
        fake.tip(TIP, BLOCK_UNIX_SECONDS);
        fake.header(BLOCK_UNIX_SECONDS);
        fake.history(&[(payment.compute_txid(), i32::try_from(MINED_AT)?)]);
        fake.unspent(&[(payment.compute_txid(), 0, RECEIVED_SAT, MINED_AT)]);
        Ok(Chain { ours, theirs, funding, payment, fake })
    }

    impl Chain {
        /// A pass fetches the history's transactions and then the parents it needs to
        /// know the fee and the sender, so the fake answers one call at a time.
        fn serve_two_rounds(&self) {
            self.fake.queue_transactions(from_ref(&self.payment));
            self.fake.queue_transactions(from_ref(&self.funding));
        }

        fn watched(&self, cursor: Option<SyncCursor>) -> Vec<WatchedScript<'_>> {
            vec![WatchedScript { address_id: 7, script: self.ours.script(), cursor, pending: &[] }]
        }

        async fn client(&self) -> Fallible<ElectrumClient> {
            self.fake.server.serves_bitcoin();
            Ok(self.fake.server.client().await?)
        }
    }

    #[tokio::test]
    async fn one_pass_reads_the_balance_the_unspent_set_and_the_history() {
        let chain = chain().unwrap();
        chain.serve_two_rounds();
        let client = chain.client().await.unwrap();
        let events = FakeEventSink::new();
        let outcome = sync_pass(&BITCOIN, &client, &chain.watched(None), &events).await.unwrap();

        assert_eq!(outcome.tip_height, TIP);
        assert_eq!(outcome.addresses.len(), 1);
        let address = outcome.addresses.first().expect("one address");
        assert_eq!(address.address_id, 7);
        assert!(!address.is_unchanged);
        assert_eq!(Utxo::total(&address.coins).unwrap(), units(RECEIVED_SAT));
        assert_eq!(address.coins.len(), 1);
        let coin = address.coins.first().expect("a coin");
        assert_eq!(coin.value(), units(RECEIVED_SAT));
        assert_eq!(ScriptKind::from(coin.kind()), ScriptKind::P2wpkh);
        assert_eq!(coin.height(), Some(MINED_AT));

        assert_eq!(address.records.len(), 1);
        let record = address.records.first().expect("a record");
        assert_eq!(record.txid.value(), chain.payment.compute_txid().to_string());
        assert_eq!(record.direction, TxDirection::Incoming);
        assert_eq!(record.amount, units(RECEIVED_SAT));
        assert_eq!(record.fee, Some(units(FUNDED_SAT - RECEIVED_SAT)));
        assert_eq!(record.counterparty.as_deref(), Some(chain.theirs.address()));
        assert_eq!(record.height, Some(u64::from(MINED_AT)));
        assert_eq!(record.timestamp, Some(i64::from(BLOCK_UNIX_SECONDS)));
        assert_eq!(record.confirmations, confirmations(Some(u64::from(MINED_AT)), u64::from(TIP)));

        assert!(address.refused.is_empty(), "{:?}", address.refused);
        assert!(address.cursor.digest().is_some());
        assert_eq!(address.cursor.balance(), units(RECEIVED_SAT));
        assert_eq!(outcome.live_outpoints(), vec![coin.outpoint()]);
    }

    #[tokio::test]
    async fn a_pass_emits_progress_and_never_the_terminal_state() {
        let chain = chain().unwrap();
        // Three addresses, so there is progress to emit between them.
        let others = [
            test_key(&BITCOIN, ScriptKind::P2wpkh, 3).unwrap(),
            test_key(&BITCOIN, ScriptKind::P2wpkh, 4).unwrap(),
        ];
        chain.serve_two_rounds();
        let client = chain.client().await.unwrap();
        let events = FakeEventSink::new();

        let mut watched = chain.watched(None);
        for (index, key) in others.iter().enumerate() {
            watched.push(WatchedScript {
                address_id: i64::try_from(index).unwrap() + 20,
                script: key.script(),
                cursor: None,
                pending: &[],
            });
        }
        let outcome = sync_pass(&BITCOIN, &client, &watched, &events).await.unwrap();

        let emitted = events.drain();
        assert_eq!(emitted.len(), watched.len(), "one before each address");
        let mut last: Option<u16> = None;
        for event in &emitted {
            let WalletEvent::SyncProgress(progress) = event else { panic!("{event:?}") };
            assert!(progress.progress_permille() < ChainSyncProgress::MAX_PERMILLE);
            assert!(
                last.is_none_or(|previous| progress.progress_permille() > previous),
                "progress only ever advances"
            );
            assert_eq!(progress.tip_height(), Some(u64::from(TIP)));
            last = Some(progress.progress_permille());
        }
        assert_eq!(
            emitted.first().and_then(|event| match event {
                WalletEvent::SyncProgress(progress) => Some(progress.progress_permille()),
                WalletEvent::BalanceChanged(_)
                | WalletEvent::TxDetected(_)
                | WalletEvent::TxUpdated(_)
                | WalletEvent::TxConfirmed(_) => None,
            }),
            Some(0),
            "nothing is done before the first address"
        );
        assert_eq!(outcome.progress(&BITCOIN).progress_permille(), ChainSyncProgress::MAX_PERMILLE);
        assert_eq!(outcome.progress(&BITCOIN).synced_height(), Some(u64::from(TIP)));
    }

    #[tokio::test]
    async fn an_unchanged_history_restores_from_the_cursor_without_refetching() {
        let chain = chain().unwrap();
        chain.serve_two_rounds();
        let cursor = {
            let client = chain.client().await.unwrap();
            let events = FakeEventSink::new();
            let outcome =
                sync_pass(&BITCOIN, &client, &chain.watched(None), &events).await.unwrap();
            outcome.addresses.first().expect("one address").cursor.clone()
        };

        let client = chain.client().await.unwrap();
        let events = FakeEventSink::new();
        let before = chain.fake.server.calls().len();
        let outcome = sync_pass(&BITCOIN, &client, &chain.watched(Some(cursor.clone())), &events)
            .await
            .unwrap();

        let address = outcome.addresses.first().expect("one address");
        assert!(address.is_unchanged);
        assert!(address.records.is_empty(), "nothing was refetched, so nothing was reclassified");
        assert_eq!(Utxo::total(&address.coins).unwrap(), units(RECEIVED_SAT));
        assert_eq!(address.coins.len(), 1);
        assert_eq!(address.cursor, cursor);

        let after: Vec<Method> =
            chain.fake.server.calls().into_iter().skip(before).map(|call| call.method).collect();
        assert!(
            !after
                .iter()
                .any(|method| matches!(method, Method::GetTransaction | Method::ListUnspent)),
            "{after:?}"
        );
    }

    /// A later session has the cursor but not the pending transactions an earlier
    /// one decoded, and a replacement needs them, so it fetches the address again.
    #[tokio::test]
    async fn a_mempool_entry_this_session_has_not_decoded_is_fetched_though_the_digest_matches() {
        let chain = chain().unwrap();
        let txid = chain.payment.compute_txid();
        chain.fake.history(&[(txid, 0)]);
        chain.fake.unspent(&[(txid, 0, RECEIVED_SAT, 0)]);
        chain.serve_two_rounds();
        let client = chain.client().await.unwrap();
        let events = FakeEventSink::new();
        let first = sync_pass(&BITCOIN, &client, &chain.watched(None), &events).await.unwrap();
        let decoded = first.addresses.first().expect("one address").clone();
        assert_eq!(decoded.pending.iter().map(|tx| tx.txid).collect::<Vec<Txid>>(), vec![txid]);

        let known: Vec<WatchedScript<'_>> = chain
            .watched(Some(decoded.cursor.clone()))
            .into_iter()
            .map(|entry| WatchedScript { pending: &decoded.pending, ..entry })
            .collect();
        let quiet = sync_pass(&BITCOIN, &client, &known, &events).await.unwrap();
        let address = quiet.addresses.first().expect("one address");
        assert!(address.is_unchanged);
        assert_eq!(address.pending, decoded.pending, "the pending set carries over");

        chain.serve_two_rounds();
        let later = chain.watched(Some(decoded.cursor));
        let refetched = sync_pass(&BITCOIN, &client, &later, &events).await.unwrap();
        let address = refetched.addresses.first().expect("one address");
        assert!(!address.is_unchanged);
        assert_eq!(address.pending, decoded.pending);
    }

    /// A replacement of the original was mined after the wallet spent the
    /// original's change to another of its addresses.
    ///
    /// The original is known only from its cursor, as after a restart, and the
    /// later payment dies with it at both addresses.
    #[test]
    fn whatever_spends_a_replaced_transaction_can_never_confirm_at_any_address() {
        let chain = chain().unwrap();
        let other = test_key(&BITCOIN, ScriptKind::P2wpkh, 3).unwrap();
        let original = tx_spending(
            &chain.payment,
            0,
            &[(chain.theirs.script_pubkey(), SENT_SAT), (chain.ours.script_pubkey(), CHANGE_SAT)],
        );
        let later = tx_spending(&original, 1, &[(other.script_pubkey(), RETURNED_SAT)]);
        let replacement =
            tx_spending(&chain.payment, 0, &[(chain.theirs.script_pubkey(), SENT_SAT)]);
        let pending = |tx: &Transaction, address_id, direction| PendingTx {
            txid: tx.compute_txid(),
            spends: tx.input.iter().map(|txin| txin.previous_output).collect(),
            record: TxRecord::builder(
                AssetId::Native(BITCOIN.chain),
                address_id,
                TxId::new(BITCOIN.chain, tx.compute_txid().to_string()),
                direction,
                Amount::ZERO,
            )
            .build(),
        };
        let remembered = [&original, &later]
            .map(|tx| CursorPending::from(&pending(tx, 7, TxDirection::Outgoing)))
            .to_vec();
        let cursor = SyncCursor::new(None, &[], &[], remembered);
        let decoded = [pending(&later, 8, TxDirection::Incoming)];
        let watched = [
            WatchedScript {
                address_id: 7,
                script: chain.ours.script(),
                cursor: Some(cursor),
                pending: &[],
            },
            WatchedScript {
                address_id: 8,
                script: other.script(),
                cursor: None,
                pending: &decoded,
            },
        ];
        let mined = HistoryEntry {
            txid: replacement.compute_txid(),
            confirmation: Confirmation::Confirmed { height: MINED_AT },
            fee: None,
        };
        let histories = [vec![mined], Vec::new()];

        let txs = BTreeMap::from([(replacement.compute_txid(), replacement)]);
        assert_eq!(
            replaced_pending(&watched, &histories, &txs),
            BTreeSet::from([original.compute_txid(), later.compute_txid()])
        );
        assert!(
            replaced_pending(&watched, &histories, &BTreeMap::new()).is_empty(),
            "a transaction gone from the history with nothing seen to conflict with it may \
             still confirm"
        );
    }

    #[tokio::test]
    async fn a_history_that_moved_is_refetched() {
        let chain = chain().unwrap();
        let stale = SyncCursor::new(None, &[], &[], Vec::new());
        chain.serve_two_rounds();
        let client = chain.client().await.unwrap();
        let events = FakeEventSink::new();
        let outcome =
            sync_pass(&BITCOIN, &client, &chain.watched(Some(stale)), &events).await.unwrap();
        let address = outcome.addresses.first().expect("one address");
        assert!(!address.is_unchanged);
        assert_eq!(address.records.len(), 1);
    }

    #[tokio::test]
    async fn a_history_that_spends_as_well_as_receives_classifies_every_record() {
        let ours = test_key(&BITCOIN, ScriptKind::P2wpkh, 1).unwrap();
        let theirs = test_key(&BITCOIN, ScriptKind::P2wpkh, 2).unwrap();

        // A stranger funds us, we pay them and keep change, then some blocks later we
        // consolidate the change back to ourselves. The fake answers every height with
        // one header, so the two heights are not adjacent.
        let root = tx_paying(&[(theirs.script_pubkey(), FUNDED_SAT)]);
        let incoming = tx_spending(&root, 0, &[(ours.script_pubkey(), RECEIVED_SAT)]);
        let outgoing = tx_spending(
            &incoming,
            0,
            &[(theirs.script_pubkey(), SENT_SAT), (ours.script_pubkey(), CHANGE_SAT)],
        );
        let self_pay = tx_spending(&outgoing, 1, &[(ours.script_pubkey(), RETURNED_SAT)]);

        let fake = FakeChain::new();
        fake.tip(TIP, BLOCK_UNIX_SECONDS);
        fake.header(BLOCK_UNIX_SECONDS);
        let height = i32::try_from(MINED_AT).unwrap();
        fake.history(&[
            (incoming.compute_txid(), height),
            (outgoing.compute_txid(), height),
            (self_pay.compute_txid(), height.saturating_add(10)),
        ]);
        fake.unspent(&[(self_pay.compute_txid(), 0, RETURNED_SAT, MINED_AT.saturating_add(10))]);
        // The pass asks for the history's transactions in transaction-id order, then
        // for the one parent it still needs to price them.
        let mut history = vec![incoming.clone(), outgoing.clone(), self_pay.clone()];
        history.sort_by_key(Transaction::compute_txid);
        fake.queue_transactions(&history);
        fake.queue_transactions(from_ref(&root));

        fake.server.serves_bitcoin();
        let client = fake.server.client().await.unwrap();
        let events = FakeEventSink::new();
        let watched = vec![WatchedScript {
            address_id: 7,
            script: ours.script(),
            cursor: None,
            pending: &[],
        }];
        let outcome = sync_pass(&BITCOIN, &client, &watched, &events).await.unwrap();

        let address = outcome.addresses.first().expect("one address");
        assert_eq!(Utxo::total(&address.coins).unwrap(), units(RETURNED_SAT));
        let by_txid = |tx: &Transaction| {
            let wanted = tx.compute_txid().to_string();
            address.records.iter().find(|record| record.txid.value() == wanted).cloned()
        };

        let received = by_txid(&incoming).expect("the incoming record");
        assert_eq!(received.direction, TxDirection::Incoming);
        assert_eq!(received.amount, units(RECEIVED_SAT));
        assert_eq!(received.counterparty.as_deref(), Some(theirs.address()));

        let paid = by_txid(&outgoing).expect("the outgoing record");
        assert_eq!(paid.direction, TxDirection::Outgoing);
        assert_eq!(paid.amount, units(SENT_SAT), "what left for somebody else, fee excluded");
        assert_eq!(paid.fee, Some(units(RECEIVED_SAT - SENT_SAT - CHANGE_SAT)));
        assert_eq!(paid.counterparty.as_deref(), Some(theirs.address()));

        let consolidated = by_txid(&self_pay).expect("the self-transfer record");
        assert_eq!(consolidated.direction, TxDirection::SelfTransfer);
        assert_eq!(consolidated.amount, units(RETURNED_SAT), "what came back");
        assert_eq!(consolidated.fee, Some(units(CHANGE_SAT - RETURNED_SAT)));
        assert_eq!(consolidated.counterparty, None, "a consolidation has no other side");
        assert_eq!(consolidated.pays_for, None, "its parent was already mined");
    }

    /// A transaction of the wallet's alone that spends the change of a send still in
    /// the mempool is a speed-up of that send, whatever it pays back.
    #[tokio::test]
    async fn a_child_of_a_pending_send_is_recorded_as_the_speed_up_it_is() {
        let ours = test_key(&BITCOIN, ScriptKind::P2wpkh, 1).unwrap();
        let theirs = test_key(&BITCOIN, ScriptKind::P2wpkh, 2).unwrap();
        let root = tx_paying(&[(theirs.script_pubkey(), FUNDED_SAT)]);
        let incoming = tx_spending(&root, 0, &[(ours.script_pubkey(), RECEIVED_SAT)]);
        let outgoing = tx_spending(
            &incoming,
            0,
            &[(theirs.script_pubkey(), SENT_SAT), (ours.script_pubkey(), CHANGE_SAT)],
        );
        let child = tx_spending(&outgoing, 1, &[(ours.script_pubkey(), RETURNED_SAT)]);

        let fake = FakeChain::new();
        fake.tip(TIP, BLOCK_UNIX_SECONDS);
        fake.header(BLOCK_UNIX_SECONDS);
        fake.history(&[
            (incoming.compute_txid(), i32::try_from(MINED_AT).unwrap()),
            (outgoing.compute_txid(), 0),
            (child.compute_txid(), -1),
        ]);
        fake.unspent(&[(child.compute_txid(), 0, RETURNED_SAT, 0)]);
        let mut history = vec![incoming.clone(), outgoing.clone(), child.clone()];
        history.sort_by_key(Transaction::compute_txid);
        fake.queue_transactions(&history);
        fake.queue_transactions(from_ref(&root));

        fake.server.serves_bitcoin();
        let client = fake.server.client().await.unwrap();
        let watched = vec![WatchedScript {
            address_id: 7,
            script: ours.script(),
            cursor: None,
            pending: &[],
        }];
        let outcome = sync_pass(&BITCOIN, &client, &watched, &FakeEventSink::new()).await.unwrap();
        let address = outcome.addresses.first().expect("one address");
        let wanted = child.compute_txid().to_string();
        let record = address.records.iter().find(|record| record.txid.value() == wanted).unwrap();
        assert_eq!(
            record.pays_for,
            Some(TxId::new(BITCOIN.chain, outgoing.compute_txid().to_string()))
        );
        assert_eq!(record.direction, TxDirection::SelfTransfer);
        assert!(record.amount.is_zero(), "the change it pays back is not an amount moved");
        assert_eq!(record.fee, Some(units(CHANGE_SAT - RETURNED_SAT)));
        let paid = address
            .records
            .iter()
            .find(|record| record.txid.value() == outgoing.compute_txid().to_string())
            .unwrap();
        assert_eq!((paid.amount, paid.pays_for.as_ref()), (units(SENT_SAT), None));
    }

    #[tokio::test]
    async fn a_short_batch_fails_the_pass_instead_of_persisting_a_zero() {
        let chain = chain().unwrap();
        // Two history entries make a batch of two, which the server answers with one.
        let mined = i32::try_from(MINED_AT).unwrap();
        chain.fake.history(&[
            (chain.payment.compute_txid(), mined),
            (chain.funding.compute_txid(), mined),
        ]);
        chain.fake.server.always(
            Method::GetTransaction,
            Answer::Each(vec![format!("\"{}\"", hex::encode(serialize(&chain.payment)))]),
        );
        let client = chain.client().await.unwrap();
        let events = FakeEventSink::new();
        let outcome = sync_pass(&BITCOIN, &client, &chain.watched(None), &events).await;
        assert!(
            matches!(
                outcome,
                Err(UtxoError::Electrum(ElectrumError::BatchLength {
                    requested: 2,
                    received: 1,
                    ..
                }))
            ),
            "a short answer must not become an empty history: {outcome:?}"
        );
    }

    #[tokio::test]
    async fn an_address_with_no_history_syncs_to_an_empty_set() {
        let chain = chain().unwrap();
        chain.fake.history(&[]);
        chain.fake.unspent(&[]);
        let client = chain.client().await.unwrap();
        let events = FakeEventSink::new();
        let outcome = sync_pass(&BITCOIN, &client, &chain.watched(None), &events).await.unwrap();

        let address = outcome.addresses.first().expect("one address");
        assert!(address.coins.is_empty());
        assert!(address.records.is_empty());
        assert_eq!(address.cursor.digest(), None);
        assert_eq!(address.cursor.balance(), Amount::ZERO);
    }

    #[tokio::test]
    async fn a_pass_with_nothing_to_watch_still_reads_the_tip() {
        let chain = chain().unwrap();
        let client = chain.client().await.unwrap();
        let events = FakeEventSink::new();
        let outcome = sync_pass(&BITCOIN, &client, &[], &events).await.unwrap();
        assert_eq!(outcome.tip_height, TIP);
        assert!(outcome.addresses.is_empty());
        assert!(outcome.live_outpoints().is_empty());
        assert!(events.drain().is_empty());
    }
}
