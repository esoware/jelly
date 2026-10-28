//! Helpers return `Fallible` instead of panicking, so a broken fixture says what was
//! wrong.
//!
//! The adapter suites drive the shipped adapter over an in-memory Electrum server that
//! speaks the real wire protocol, so framing, batching, failover, the chain guard, and
//! cursor routing all run as they would over a socket. The guard checks each table's
//! real block zero and checkpoint.

use std::collections::BTreeMap;
use std::error::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bitcoin::absolute::LockTime;
use bitcoin::block::Header;
use bitcoin::consensus::deserialize;
use bitcoin::transaction::Version;
use bitcoin::{OutPoint, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Txid, Witness};
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, ChainSpec, ChainSyncProgress, Clock,
    Deadline, EndpointLabel, FeeQuote, FeeRate, FeeSelection, ScriptKind, Transport, TxRequest,
    UnsignedTx, WalletEvent, WatchedAddress,
    test_util::{
        FakeClock, FakeEventSink, MemoryCursorStore, TxRequestBuilder, WatchedAddressBuilder,
    },
};
use jelly_electrum::{
    ElectrumClient, Method,
    test_util::{Answer, ENDPOINT_URL, bitcoin_genesis_header},
};
use jelly_net::{Endpoint, NetError, NetRuntime, PacerPolicy, Pool};
use jelly_utxo::{
    Auction, FeeApi, FeeMarket, RecommendedFees, UtxoAdapter, UtxoError, UtxoParams,
    test_util::{
        DecodedPayload, FakeChain, ListedUnspent, TestKey, decode_payload, test_key, tx_paying,
        tx_spending,
    },
};
use serde_json::Value;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error>>;

/// A tip deep enough that no confirmation filter drops a fixture coin.
pub(crate) const TIP: u32 = 800_000;

/// A build never shuffles outputs, so the payment and change always land here.
pub(crate) const RECIPIENT_VOUT: u32 = 0;
pub(crate) const CHANGE_VOUT: u32 = 1;

/// The height the fixture history is mined at, far enough below the tip that a
/// record is out of the confirmation watch window.
pub(crate) const MINED_AT: u32 = 799_900;
/// Not adjacent to [`MINED_AT`], because the fake answers every height with the
/// same header and the client refuses two adjacent headers that don't chain.
pub(crate) const MINED_LATER: u32 = 799_950;
pub(crate) const BLOCK_UNIX_SECONDS: u32 = 1_700_000_000;

/// Litecoin's real block-zero header, consensus encoded. `tests/it/chains.rs` proves
/// it hashes to [`LITECOIN`]'s genesis.
pub(crate) const LITECOIN_BLOCK_ZERO_HEADER: &str = "010000000000000000000000000000000000000000000000000000000000000000000000d9ced4ed1130f7b7faad9be25323ffafa33232a17c3edf6cfd97bee6bafbdd97b9aa8e4ef0ff0f1ecd513f7c";

/// The recorded headers the other tables prove, from `tests/vectors/headers.json`.
/// `tests/it/chains.rs` proves each hashes to its table's block.
const HEADERS: &str = include_str!("../../vectors/headers.json");

/// One base unit per virtual byte, which a pre-29.1 Bitcoin Core node reports and
/// Litecoin uses as its relay floor. It's at or above both compiled floors.
const FLOOR: &str =
    r#"{"mempoolminfee":0.00001,"minrelaytxfee":0.00001,"incrementalrelayfee":0.00001}"#;
/// Five base units per virtual byte, above Bitcoin's wallet floor and below
/// Litecoin's.
const ESTIMATE: &str = "0.00005000";

pub(crate) const BITCOIN: UtxoParams = jelly_bitcoin::PARAMS;
pub(crate) const LITECOIN: UtxoParams = jelly_litecoin::PARAMS;
pub(crate) const BITCOIN_CASH: UtxoParams = jelly_bitcoin_cash::PARAMS;
pub(crate) const DOGECOIN: UtxoParams = jelly_dogecoin::PARAMS;

/// Every table, for a test that doesn't depend on the scale of amounts.
pub(crate) const TABLES: [UtxoParams; 4] = [BITCOIN, LITECOIN, BITCOIN_CASH, DOGECOIN];

/// The tables a test with sat-scale amounts and rates holds on. Dogecoin's floors
/// are three orders of magnitude higher, so `tests/it/dogecoin.rs` runs the same
/// flows at its own scale.
pub(crate) const CHAINS: [UtxoParams; 3] = [BITCOIN, LITECOIN, BITCOIN_CASH];

/// The tables whose fee levels follow a market, for a test of where a reading comes
/// from.
pub(crate) const AUCTIONED: [UtxoParams; 2] = [BITCOIN, LITECOIN];

pub(crate) fn sats(value: u64) -> Amount {
    Amount::from(value)
}

pub(crate) const fn kwu(value: u64) -> FeeRate {
    FeeRate::from_sat_per_kwu(value)
}

pub(crate) fn quoted(text: &str) -> String {
    format!("\"{text}\"")
}

/// The chain's recorded addresses, from `tests/vectors/<chain>/addresses.json`.
pub(crate) fn address_vectors(params: &UtxoParams) -> Fallible<Value> {
    let text = match params.chain {
        ChainId::Bitcoin => include_str!("../../vectors/bitcoin/addresses.json"),
        ChainId::Litecoin => include_str!("../../vectors/litecoin/addresses.json"),
        ChainId::BitcoinCash => include_str!("../../vectors/bitcoin_cash/addresses.json"),
        ChainId::Dogecoin => include_str!("../../vectors/dogecoin/addresses.json"),
        other @ (ChainId::Zcash
        | ChainId::Ethereum
        | ChainId::Arbitrum
        | ChainId::Base
        | ChainId::Optimism
        | ChainId::Bsc
        | ChainId::Polygon
        | ChainId::Avalanche
        | ChainId::Solana
        | ChainId::Tron
        | ChainId::Xrp
        | ChainId::Ton
        | ChainId::Monero) => return Err(format!("{other} has no utxo table").into()),
    };
    Ok(serde_json::from_str(text)?)
}

/// The chain's recorded fee answers and the rates they produce, from
/// `tests/vectors/<chain>/fee_rates.json`. Only an auctioned chain reads a market.
pub(crate) fn fee_rate_vectors(params: &UtxoParams) -> Fallible<Value> {
    let text = if params.chain == ChainId::Litecoin {
        include_str!("../../vectors/litecoin/fee_rates.json")
    } else {
        include_str!("../../vectors/bitcoin/fee_rates.json")
    };
    Ok(serde_json::from_str(text)?)
}

/// The market an auctioned table prices from.
pub(crate) fn auction(params: &UtxoParams) -> Fallible<Auction> {
    match params.fee_market {
        FeeMarket::Auction(auction) => Ok(auction),
        FeeMarket::Flat => Err(format!("{} has a flat fee market", params.chain).into()),
    }
}

/// The script kind a test's addresses use: native segwit where the chain has it,
/// P2PKH where it doesn't.
pub(crate) fn kind_of(params: &UtxoParams) -> ScriptKind {
    if params.script_kinds.contains(&ScriptKind::P2wpkh) {
        ScriptKind::P2wpkh
    } else {
        ScriptKind::P2pkh
    }
}

pub(crate) fn header_vector(name: &str) -> Fallible<String> {
    let file: Value = serde_json::from_str(HEADERS)?;
    Ok(text(&file, name))
}

pub(crate) fn rows(value: &Value, pointer: &str) -> Vec<Value> {
    value.pointer(pointer).and_then(Value::as_array).cloned().unwrap_or_default()
}

pub(crate) fn text(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

pub(crate) fn number(value: &Value, key: &str) -> u64 {
    value.get(key).and_then(Value::as_u64).unwrap_or_default()
}

/// The spec of the chain crate `params` comes from.
pub(crate) fn spec(params: &UtxoParams) -> Fallible<&'static ChainSpec> {
    match params.chain {
        ChainId::Bitcoin => Ok(&jelly_bitcoin::SPEC),
        ChainId::Litecoin => Ok(&jelly_litecoin::SPEC),
        ChainId::BitcoinCash => Ok(&jelly_bitcoin_cash::SPEC),
        ChainId::Dogecoin => Ok(&jelly_dogecoin::SPEC),
        other @ (ChainId::Zcash
        | ChainId::Ethereum
        | ChainId::Arbitrum
        | ChainId::Base
        | ChainId::Optimism
        | ChainId::Bsc
        | ChainId::Polygon
        | ChainId::Avalanche
        | ChainId::Solana
        | ChainId::Tron
        | ChainId::Xrp
        | ChainId::Ton
        | ChainId::Monero) => Err(format!("{other} has no utxo table").into()),
    }
}

/// The block-zero header a server on the chain answers height zero with.
pub(crate) fn genesis_header(params: &UtxoParams) -> String {
    if params.chain == ChainId::Litecoin {
        LITECOIN_BLOCK_ZERO_HEADER.to_owned()
    } else if params.chain == ChainId::Dogecoin {
        header_vector("dogecoin0").unwrap_or_default()
    } else {
        bitcoin_genesis_header()
    }
}

/// The header a server on the chain answers its checkpoint height with, for a
/// table that names one.
pub(crate) fn checkpoint_header(params: &UtxoParams) -> Option<String> {
    params.checkpoint?;
    if params.chain == ChainId::BitcoinCash {
        header_vector("bitcoinCash840000").ok()
    } else if params.chain == ChainId::Litecoin {
        header_vector("litecoin2520000").ok()
    } else {
        header_vector("bitcoin840000").ok()
    }
}

pub(crate) fn watched(params: &UtxoParams, id: i64, address: &str) -> WatchedAddress {
    WatchedAddressBuilder::new(id, params.chain, address).build()
}

/// An explicit rate, so the test sets the number instead of a fee source.
pub(crate) fn request(params: &UtxoParams, to: &str, amount: u64, sat_per_vb: &str) -> TxRequest {
    TxRequestBuilder::new(7, to, AssetId::Native(params.chain), sats(amount))
        .fee(FeeSelection::Rate { rate: sat_per_vb.to_owned() })
        .build()
}

/// Everything the address holds, paid to `to` at `fee`.
pub(crate) fn drain(params: &UtxoParams, to: &str, fee: FeeSelection) -> TxRequest {
    TxRequestBuilder::new(7, to, AssetId::Native(params.chain), Amount::ZERO)
        .send_max()
        .fee(fee)
        .build()
}

/// A rate and its decimal rendering are exact inverses, so this is the rate that gets
/// planned.
pub(crate) fn at(rate: FeeRate) -> FeeSelection {
    FeeSelection::Rate { rate: rate.format_sat_per_vb() }
}

/// What the build of `tx` decided, read back off its payload.
pub(crate) fn payload(tx: &UnsignedTx) -> Fallible<DecodedPayload> {
    Ok(decode_payload(tx.payload.as_bytes())?)
}

/// The rate a quote settled on, read back out of its custom fee.
pub(crate) fn quoted_rate(quote: &FeeQuote) -> Fallible<FeeRate> {
    let Some(FeeSelection::Rate { rate: ref sat_per_vb }) = quote.custom else {
        return Err(format!("{:?} is not a utxo fee", quote.custom).into());
    };
    Ok(FeeRate::parse_sat_per_vb(sat_per_vb)?)
}

/// A fee service that names the same rate at every level.
pub(crate) const fn every_level_at(rate: FeeRate, minimum: Option<FeeRate>) -> RecommendedFees {
    RecommendedFees { hour: rate, half_hour: rate, fastest: rate, minimum }
}

pub(crate) fn coinbase_paying(
    script_pubkey: ScriptBuf,
    value: u64,
    version: Version,
) -> Transaction {
    Transaction {
        version,
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: OutPoint::null(),
            script_sig: ScriptBuf::from(vec![0x03, 0x01, 0x02, 0x03]),
            sequence: Sequence::MAX,
            witness: Witness::new(),
        }],
        output: vec![TxOut { value: bitcoin::Amount::from_sat(value), script_pubkey }],
    }
}

/// A fee service with a fixed answer, so a whole quote is driven without a
/// socket.
#[derive(Debug)]
pub(crate) struct FakeFeeApi {
    answer: Option<RecommendedFees>,
    calls: AtomicU64,
}

impl FakeFeeApi {
    pub(crate) const fn answering(answer: RecommendedFees) -> Self {
        Self { answer: Some(answer), calls: AtomicU64::new(0) }
    }

    /// A service that is reachable and refuses to answer.
    pub(crate) const fn silent() -> Self {
        Self { answer: None, calls: AtomicU64::new(0) }
    }

    pub(crate) fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl FeeApi for FakeFeeApi {
    async fn recommended(&self) -> Result<RecommendedFees, UtxoError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.answer.ok_or_else(|| {
            UtxoError::Net(NetError::Status {
                endpoint: EndpointLabel::new("https", "fees.invalid", None),
                status: 503,
            })
        })
    }
}

#[derive(Debug)]
pub(crate) struct Setup {
    pub(crate) params: UtxoParams,
    pub(crate) fees: Option<Arc<dyn FeeApi>>,
    /// The header the server answers height zero with. The chain guard hashes it.
    pub(crate) genesis_hex: String,
    /// The header it answers the table's checkpoint height with, when the table
    /// names one.
    pub(crate) checkpoint_hex: Option<String>,
}

impl Setup {
    /// A server on the table's own chain, with the default configuration and no
    /// fee service.
    pub(crate) fn on(params: &UtxoParams) -> Self {
        Self {
            params: *params,
            fees: None,
            genesis_hex: genesis_header(params),
            checkpoint_hex: checkpoint_header(params),
        }
    }

    pub(crate) fn with_fees(params: &UtxoParams, api: Arc<FakeFeeApi>) -> Self {
        let fees: Arc<dyn FeeApi> = api;
        Self { fees: Some(fees), ..Self::on(params) }
    }
}

#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) params: UtxoParams,
    pub(crate) adapter: UtxoAdapter,
    pub(crate) chain: FakeChain,
    pub(crate) cursors: Arc<MemoryCursorStore>,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) events: FakeEventSink,
}

impl Harness {
    pub(crate) fn new(setup: Setup) -> Fallible<Self> {
        Self::over(setup, Arc::new(MemoryCursorStore::new()))
    }

    /// A harness over cursors an earlier one wrote, modeling a later session of the
    /// same wallet.
    pub(crate) fn over(setup: Setup, cursors: Arc<MemoryCursorStore>) -> Fallible<Self> {
        Self::with_store(setup, Arc::<MemoryCursorStore>::clone(&cursors), cursors)
    }

    pub(crate) fn with_store(
        setup: Setup,
        cursors: Arc<MemoryCursorStore>,
        store: Arc<dyn jelly_core::CursorStore>,
    ) -> Fallible<Self> {
        let params = setup.params;
        let clock = Arc::new(FakeClock::default());
        let as_clock: Arc<dyn Clock> = Arc::<FakeClock>::clone(&clock);
        let chain = FakeChain::new();
        // The identity probe asks for height zero and the checkpoint in one batch
        // before anything else, so their headers queue ahead of the standing header a
        // pass reads. The guard checks the features answer against the hash.
        let proof: Vec<String> = [Some(&setup.genesis_hex), setup.checkpoint_hex.as_ref()]
            .into_iter()
            .flatten()
            .map(|hex| quoted(hex))
            .collect();
        chain.server.on(Method::BlockHeader, Answer::Each(proof));
        chain.server.always(
            Method::ServerFeatures,
            Answer::Result(format!(r#"{{"genesis_hash":"{}"}}"#, header_hash(&setup.genesis_hex)?)),
        );
        // Standing answers, because a failed call would put the endpoint in a cooldown
        // the frozen clock never leaves. A test that wants a different reading
        // replaces them.
        chain.server.always(Method::MempoolInfo, Answer::Result(FLOOR.to_owned()));
        chain.server.always(Method::EstimateFee, Answer::Result(ESTIMATE.to_owned()));

        // The real chain guard, so a test can serve another chain's header and watch
        // the endpoint be refused.
        let connector = Arc::new(chain.server.connector(params.electrum_chain()));
        let runtime = NetRuntime::with_pacing(Arc::clone(&as_clock), PacerPolicy::UNTHROTTLED);
        let endpoint = Endpoint::parse(params.chain, Transport::ElectrumSsl, ENDPOINT_URL)?;
        let pool: Arc<Pool<ElectrumClient>> =
            Arc::new(runtime.pool(params.chain, Transport::ElectrumSsl, connector, vec![endpoint]));

        Ok(Self {
            params,
            adapter: UtxoAdapter::new(&params, pool, store, as_clock, setup.fees)?,
            chain,
            cursors,
            clock,
            events: FakeEventSink::new(),
        })
    }

    /// A deadline that never expires while the fake clock stands still.
    pub(crate) fn deadline(&self) -> Deadline {
        self.clock.deadline_in(Duration::from_secs(60))
    }

    pub(crate) async fn watch(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        self.adapter.set_watch_set(addresses).await
    }

    pub(crate) async fn sync(&self) -> Result<ChainSyncProgress, ChainError> {
        self.adapter.sync(self.deadline(), &self.events).await
    }

    pub(crate) fn drain(&self) -> Vec<WalletEvent> {
        self.events.drain()
    }

    /// A send on this harness's chain at an explicit rate.
    pub(crate) fn request(&self, to: &str, amount: u64, sat_per_vb: &str) -> TxRequest {
        request(&self.params, to, amount, sat_per_vb)
    }

    pub(crate) fn key(&self, kind: ScriptKind, seed: u8) -> Fallible<TestKey> {
        Ok(test_key(&self.params, kind, seed)?)
    }

    /// A standing answer for `method`, for every call and every batch element.
    pub(crate) fn serve(&self, method: Method, json: &str) {
        self.chain.server.always(method, Answer::Result(json.to_owned()));
    }

    /// Queues one `blockchain.transaction.get` answer, sorted by txid as a batch
    /// asks for them.
    pub(crate) fn queue_transactions(&self, txs: &[Transaction]) {
        let mut sorted = txs.to_vec();
        sorted.sort_by_key(Transaction::compute_txid);
        self.chain.queue_transactions(&sorted);
    }

    pub(crate) fn calls(&self, method: Method) -> usize {
        self.chain.server.calls_to(method).len()
    }

    pub(crate) fn forget_calls(&self) {
        self.chain.server.forget_calls();
    }
}

/// The hash of a consensus-encoded header, as a server advertises it.
fn header_hash(hex_text: &str) -> Fallible<String> {
    let bytes = hex::decode(hex_text)?;
    let header: Header = deserialize(&bytes)?;
    Ok(header.block_hash().to_string())
}

/// One address of ours, one stranger, and the two transactions that put coins on
/// ours.
///
/// Two transactions is the smallest history with a knowable fee and sender, since
/// the funding transaction gives the payment's input value.
#[derive(Debug)]
pub(crate) struct Wallet {
    pub(crate) params: UtxoParams,
    pub(crate) ours: TestKey,
    pub(crate) theirs: TestKey,
    pub(crate) funding: Transaction,
    pub(crate) payment: Transaction,
    pub(crate) values: Vec<u64>,
}

impl Wallet {
    /// A wallet holding one coin per entry of `values`, all on one address.
    pub(crate) fn new(params: &UtxoParams, kind: ScriptKind, values: &[u64]) -> Fallible<Self> {
        let ours = test_key(params, kind, 1)?;
        let theirs = test_key(params, kind_of(params), 2)?;
        let total: u64 = values.iter().copied().sum();
        let funding = tx_paying(&[(theirs.script_pubkey(), total.saturating_add(10_000))]);
        let outputs: Vec<(ScriptBuf, u64)> =
            values.iter().map(|value| (ours.script_pubkey(), *value)).collect();
        let payment = tx_spending(&funding, 0, &outputs);
        Ok(Self { params: *params, ours, theirs, funding, payment, values: values.to_vec() })
    }

    pub(crate) fn address(&self) -> WatchedAddress {
        watched(&self.params, 7, self.ours.address())
    }

    pub(crate) fn total(&self) -> u64 {
        self.values.iter().copied().sum()
    }

    /// Every coin confirmed at [`MINED_AT`].
    pub(crate) fn ledger(&self) -> Ledger {
        let mut ledger = Ledger::new().known(&self.funding).confirmed(&self.payment, MINED_AT);
        for (index, value) in self.values.iter().enumerate() {
            let vout = u32::try_from(index).unwrap_or_default();
            ledger = ledger.unspent(&self.payment, vout, *value, MINED_AT);
        }
        ledger
    }
}

/// What one server knows, its transactions and the watched address's history and
/// unspent set.
///
/// Every transaction it knows is answered by txid, so a pass may ask for any of
/// them in any batch, the way a real server serves them.
#[derive(Debug, Default)]
pub(crate) struct Ledger {
    known: Vec<Transaction>,
    history: Vec<(Txid, i32)>,
    unspent: Vec<ListedUnspent>,
}

impl Ledger {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// A transaction the server has but the history doesn't name, such as a parent.
    #[must_use]
    pub(crate) fn known(mut self, tx: &Transaction) -> Self {
        self.known.push(tx.clone());
        self
    }

    #[must_use]
    pub(crate) fn confirmed(mut self, tx: &Transaction, height: u32) -> Self {
        self.history.push((tx.compute_txid(), i32::try_from(height).unwrap_or_default()));
        self.known.push(tx.clone());
        self
    }

    /// A history entry whose transaction the server won't return, for an answer that
    /// comes back shorter than the batch that asked for it.
    #[must_use]
    pub(crate) fn unfetchable(mut self, tx: &Transaction, height: u32) -> Self {
        self.history.push((tx.compute_txid(), i32::try_from(height).unwrap_or_default()));
        self
    }

    /// A history entry still in the mempool, all of whose inputs are confirmed.
    #[must_use]
    pub(crate) fn mempool(mut self, tx: &Transaction) -> Self {
        self.history.push((tx.compute_txid(), 0));
        self.known.push(tx.clone());
        self
    }

    #[must_use]
    pub(crate) fn unspent(mut self, tx: &Transaction, vout: u32, value: u64, height: u32) -> Self {
        self.unspent.push((tx.compute_txid(), vout, value, height));
        self
    }

    /// An unspent output whose funding transaction is still in the mempool.
    #[must_use]
    pub(crate) fn unconfirmed(self, tx: &Transaction, vout: u32, value: u64) -> Self {
        self.unspent(tx, vout, value, 0)
    }

    /// Scripts `harness`'s server with everything one pass over this ledger reads.
    pub(crate) fn serve(&self, harness: &Harness) {
        harness.chain.tip(TIP, BLOCK_UNIX_SECONDS);
        harness.chain.header(BLOCK_UNIX_SECONDS);
        harness.chain.history(&self.history);
        harness.chain.unspent(&self.unspent);
        harness.chain.transactions(&self.known);
    }
}

/// What one server knows about several watched addresses, each with a history and
/// an unspent set of its own, answered by script hash as a real server answers.
#[derive(Debug, Clone, Default)]
pub(crate) struct ScriptLedger {
    known: Vec<Transaction>,
    histories: BTreeMap<ScriptBuf, Vec<(Txid, i32)>>,
    unspents: BTreeMap<ScriptBuf, Vec<ListedUnspent>>,
}

impl ScriptLedger {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub(crate) fn known(mut self, tx: &Transaction) -> Self {
        self.known.push(tx.clone());
        self
    }

    /// `tx` mined at `height`, in the history of every key in `keys`.
    #[must_use]
    pub(crate) fn confirmed(mut self, tx: &Transaction, height: u32, keys: &[&TestKey]) -> Self {
        for key in keys {
            self.histories
                .entry(key.script_pubkey())
                .or_default()
                .push((tx.compute_txid(), i32::try_from(height).unwrap_or_default()));
        }
        self.known.push(tx.clone());
        self
    }

    /// Output `vout` of `tx`, unspent on the address it pays, mined at `height`.
    #[must_use]
    pub(crate) fn unspent(mut self, tx: &Transaction, vout: u32, height: u32) -> Self {
        let Some(txout) = usize::try_from(vout).ok().and_then(|index| tx.output.get(index)) else {
            return self;
        };
        self.unspents.entry(txout.script_pubkey.clone()).or_default().push((
            tx.compute_txid(),
            vout,
            txout.value.to_sat(),
            height,
        ));
        self
    }

    /// Scripts `harness`'s server with everything one pass over `keys` reads.
    pub(crate) fn serve(&self, harness: &Harness, keys: &[&TestKey]) {
        harness.chain.tip(TIP, BLOCK_UNIX_SECONDS);
        harness.chain.header(BLOCK_UNIX_SECONDS);
        let histories: Vec<(ScriptBuf, Vec<(Txid, i32)>)> = keys
            .iter()
            .map(|key| {
                let script = key.script_pubkey();
                let entries = self.histories.get(&script).cloned().unwrap_or_default();
                (script, entries)
            })
            .collect();
        let unspents: Vec<(ScriptBuf, Vec<ListedUnspent>)> = keys
            .iter()
            .map(|key| {
                let script = key.script_pubkey();
                let entries = self.unspents.get(&script).cloned().unwrap_or_default();
                (script, entries)
            })
            .collect();
        harness.chain.histories(&histories);
        harness.chain.unspents(&unspents);
        harness.chain.transactions(&self.known);
    }
}

/// An adapter that has run one pass over the standard ledger of `values`.
pub(crate) async fn synced(
    setup: Setup,
    kind: ScriptKind,
    values: &[u64],
) -> Fallible<(Harness, Wallet)> {
    let params = setup.params;
    let harness = Harness::new(setup)?;
    let wallet = Wallet::new(&params, kind, values)?;
    wallet.ledger().serve(&harness);
    harness.watch(&[wallet.address()]).await?;
    harness.sync().await?;
    harness.drain();
    Ok((harness, wallet))
}

pub(crate) fn decode(raw: &[u8]) -> Fallible<Transaction> {
    Ok(deserialize(raw)?)
}
