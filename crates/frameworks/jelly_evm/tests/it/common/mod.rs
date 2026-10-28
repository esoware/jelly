//! Tests drive the shipped [`EvmAdapter`] over a scripted node, so they exercise the
//! same JSON-RPC documents, decoders, nonce ledger, and cursor routing a real socket
//! would.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fs;
use std::iter::repeat_n;
use std::sync::Arc;
use std::time::Duration;

use alloy::hex;
use alloy::primitives::U256;
use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainEntry, ChainError, ChainId, ChainSyncProgress, Clock,
    CursorScope, CursorStore as _, Deadline, FeeSelection, KeyKind, KeyMaterial, ReservationSlot,
    SignedTx, TokenId, Transport, TxDetail, TxRecord, TxRequest, WalletEvent, WatchedAddress,
    test_util::{
        FakeClock, FakeEventSink, MemoryCursorStore, TxRequestBuilder, WatchedAddressBuilder,
        key_material_of,
    },
};
use jelly_evm::{
    Answered, EvmAdapter, EvmConfig, EvmError, EvmNode, EvmParams, HistoryApi, Method, RpcCall,
    test_util::{FakeHistory, endpoint},
};
use jelly_net::{
    Endpoint, HttpClient, HttpConnector, HttpEndpoint, NetError, NetRuntime, NoIdentity,
    PacerPolicy, Pool,
};
use parking_lot::Mutex;
use serde_json::value::RawValue;
use tokio::time::sleep;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error>>;

pub(crate) const ETHEREUM: &EvmParams = &jelly_ethereum::PARAMS;
pub(crate) const ARBITRUM: &EvmParams = &jelly_arbitrum::PARAMS;
pub(crate) const BASE: &EvmParams = &jelly_base::PARAMS;
pub(crate) const OPTIMISM: &EvmParams = &jelly_optimism::PARAMS;
pub(crate) const BSC: &EvmParams = &jelly_bsc::PARAMS;
pub(crate) const POLYGON: &EvmParams = &jelly_polygon::PARAMS;
pub(crate) const AVALANCHE: &EvmParams = &jelly_avalanche::PARAMS;
/// Ethereum with a confirmation target deep enough that a fixture receipt doesn't
/// retire its send in the pass that sees it.
pub(crate) static DEEP_ETHEREUM: EvmParams = EvmParams { confirmations_target: 1_000, ..*ETHEREUM };
pub(crate) const NETWORKS: [&EvmParams; 7] =
    [ETHEREUM, ARBITRUM, BASE, OPTIMISM, BSC, POLYGON, AVALANCHE];
/// The entries of the same networks, in the same order.
pub(crate) const ENTRIES: [&dyn ChainEntry; 7] = [
    jelly_ethereum::ENTRY,
    jelly_arbitrum::ENTRY,
    jelly_base::ENTRY,
    jelly_optimism::ENTRY,
    jelly_bsc::ENTRY,
    jelly_polygon::ENTRY,
    jelly_avalanche::ENTRY,
];

/// The address the fixture wallet watches.
pub(crate) const OWNER: &str = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045";
/// An address outside the wallet.
pub(crate) const PEER: &str = "0x5041ed759Dd4aFc3a72b8192C143F72f4724081A";
/// A second watched address, for the multi-address cases.
pub(crate) const SECOND: &str = "0x2bDE5dE9c6cc98380ECD3c7D0aF62592707Ae78D";
pub(crate) const USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
pub(crate) const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

/// A throwaway secp256k1 scalar. Test material only, never a wallet key.
pub(crate) const TEST_KEY: &str =
    "8f2a55949038a9610f50fb23b5883af3b4ecb3c3bb792cbcefbd1542c692be63";
/// The sender the test key owns.
pub(crate) const SIGNER: &str = "0xFE3B557E8Fb62b89F4916B721be55cEb828dBd73";
/// A second throwaway scalar, which owns a different address.
pub(crate) const OTHER_KEY: &str =
    "4646464646464646464646464646464646464646464646464646464646464646";

/// What Base's oracle quoted as the most the L1 data fee could be for a 120-byte
/// transaction.
pub(crate) const L1_FEE_WEI: u128 = 973_558_459;

pub(crate) const GWEI: u128 = 1_000_000_000;
pub(crate) const ETH: u128 = 1_000_000_000_000_000_000;

/// A tip deep enough that a fixture receipt is no longer inside the confirmation
/// watch window.
pub(crate) const TIP: u64 = 18_000_100;
pub(crate) const TIP_HEX: &str = "\"0x112a8e4\"";

pub(crate) fn key_material(hex_key: &str) -> Fallible<KeyMaterial> {
    Ok(key_material_of(KeyKind::Secp256k1, hex::decode(hex_key)?))
}

pub(crate) fn watched(id: i64, chain: ChainId, text: &str) -> WatchedAddress {
    WatchedAddressBuilder::new(id, chain, text).build()
}

/// A send of `amount` base units to `to`, at the normal fee level.
pub(crate) fn request(to: &str, asset: AssetId, amount: u128) -> TxRequest {
    TxRequestBuilder::new(7, to, asset, Amount::from_base_units(amount)).build()
}

/// A send with the fee and gas limit pinned, so the numbers under test are the
/// ones the test wrote.
pub(crate) fn pinned(
    to: &str,
    asset: AssetId,
    amount: u128,
    cap: u128,
    tip: u128,
    gas: u64,
) -> TxRequest {
    TxRequestBuilder::new(7, to, asset, Amount::from_base_units(amount))
        .fee(evm_fee(Some(gas), Some(cap), Some(tip)))
        .build()
}

/// One coin to `to` at `nonce`, with a 30 gwei cap, a 1 gwei tip, and 21,000 gas.
pub(crate) fn payment(to: &str, nonce: u64) -> TxRequest {
    TxRequestBuilder::new(7, to, ETHEREUM.native(), Amount::from_base_units(ETH))
        .fee(evm_fee(Some(21_000), Some(30 * GWEI), Some(GWEI)))
        .nonce(nonce)
        .build()
}

pub(crate) fn evm_fee(gas: Option<u64>, cap: Option<u128>, tip: Option<u128>) -> FeeSelection {
    FeeSelection::Evm {
        gas_limit: gas.map(|value| value.to_string()),
        max_fee_per_gas: cap.map(|value| value.to_string()),
        max_priority_fee_per_gas: tip.map(|value| value.to_string()),
    }
}

/// A quantity as a node writes it.
pub(crate) fn quantity(value: u128) -> String {
    format!("\"0x{value:x}\"")
}

pub(crate) fn fixture(file: &str, key: &str) -> Fallible<String> {
    let path = format!("{}/tests/fixtures/{file}", env!("CARGO_MANIFEST_DIR"));
    let body: serde_json::Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
    Ok(body.get(key).ok_or("the fixture has that key")?.to_string())
}

pub(crate) fn receipt(key: &str) -> Fallible<String> {
    fixture("get_receipt.json", key)
}

/// A number of wei rendered the way a preview renders a gas price.
pub(crate) fn gwei_text(wei: u128) -> String {
    format!("{} gwei", Amount::from_base_units(wei).format(9))
}

/// A thirty-two byte word as `eth_call` answers it.
pub(crate) fn word(value: u128) -> String {
    format!("\"0x{}\"", hex::encode(U256::from(value).to_be_bytes::<32>()))
}

/// Entries in one Ethereum address's balance sheet, the native balance and then
/// one per token.
pub(crate) const SHEET_ENTRIES: usize = ETHEREUM.tokens.len().saturating_add(1);

/// One Ethereum address's balances as [`aggregate`] takes them, `native` wei and
/// none of each token.
pub(crate) const fn sheet(native: u128) -> [Option<u128>; SHEET_ENTRIES] {
    let mut balances = [Some(0); SHEET_ENTRIES];
    balances[0] = Some(native);
    balances
}

/// The balances of `addresses` addresses on `network`, each holding `native`
/// and none of any token.
pub(crate) fn sheets_on(network: &EvmParams, addresses: usize, native: u128) -> Vec<Option<u128>> {
    let mut one = vec![Some(0); network.tokens.len()];
    one.insert(0, Some(native));
    one.repeat(addresses)
}

/// Where `token`'s balance sits in a sheet on `network`.
pub(crate) fn sheet_index(network: &EvmParams, token: TokenId) -> Fallible<usize> {
    let position =
        network.tokens.iter().position(|carried| *carried == token).ok_or("a carried token")?;
    Ok(position.saturating_add(1))
}

/// A successful receipt for `txid` mined in `block`, at 21,000 gas and 20 gwei.
pub(crate) fn receipt_for(txid: &str, block: u64) -> String {
    format!(
        r#"{{"transactionHash":"{txid}","status":"0x1","gasUsed":"0x5208",
            "effectiveGasPrice":"0x4a817c800","blockNumber":"{block:#x}",
            "blockHash":"0xaa00000000000000000000000000000000000000000000000000000000000002"}}"#
    )
}

/// One `aggregate3` answer, the native balance first and then one entry per token.
/// `None` is an entry marked as failed, which reads as an unknown balance rather
/// than zero.
pub(crate) fn aggregate(values: &[Option<u128>]) -> String {
    let rows: Vec<(bool, Vec<u8>)> = values
        .iter()
        .map(|value| match *value {
            Some(amount) => (true, U256::from(amount).to_be_bytes::<32>().to_vec()),
            None => (false, Vec::new()),
        })
        .collect();
    format!("\"0x{}\"", hex::encode(encode_call3_results(&rows)))
}

/// `(bool,bytes)[]` as `aggregate3` returns it, encoded by hand because the
/// crate's decoder is under test.
fn encode_call3_results(rows: &[(bool, Vec<u8>)]) -> Vec<u8> {
    let mut encoded = pad(U256::from(32u64));
    encoded.extend(pad(U256::from(rows.len())));
    let mut bodies: Vec<Vec<u8>> = Vec::with_capacity(rows.len());
    for (success, data) in rows {
        let mut body = pad(U256::from(u8::from(*success)));
        body.extend(pad(U256::from(64u64)));
        body.extend(pad(U256::from(data.len())));
        body.extend(data.iter().copied());
        let padding = data.len().next_multiple_of(32).saturating_sub(data.len());
        body.extend(repeat_n(0u8, padding));
        bodies.push(body);
    }
    let mut offset = rows.len().saturating_mul(32);
    for body in &bodies {
        encoded.extend(pad(U256::from(offset)));
        offset = offset.saturating_add(body.len());
    }
    for body in bodies {
        encoded.extend(body);
    }
    encoded
}

fn pad(value: U256) -> Vec<u8> {
    value.to_be_bytes::<32>().to_vec()
}

#[derive(Debug, Clone)]
pub(crate) enum Answer {
    /// The JSON `result` member, verbatim.
    Result(String),
    /// A JSON-RPC error object, classified by the shipped conversion.
    Error { code: i64, message: String },
    /// The pool has no endpoint to ask.
    Unreachable,
}

impl Answer {
    pub(crate) fn ok(json: &str) -> Self {
        Self::Result(json.to_owned())
    }

    pub(crate) fn refused(message: &str) -> Self {
        Self::Error { code: -32000, message: message.to_owned() }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) method: Method,
    pub(crate) params: String,
}

/// A node whose every answer is scripted.
///
/// Queued answers are taken in order, then the standing answer serves the rest.
/// A refusal goes through the shipped `NetError` conversion, so a test sees the
/// typed failure a real node's wording produces.
#[derive(Debug, Default)]
pub(crate) struct FakeNode {
    queued: Mutex<HashMap<Method, VecDeque<Answer>>>,
    standing: Mutex<HashMap<Method, Answer>>,
    calls: Mutex<Vec<Call>>,
    delay: Mutex<Option<Duration>>,
}

impl FakeNode {
    /// Every account has sent nothing until a test says otherwise, because every
    /// pass reads the nonces with the balances.
    pub(crate) fn new() -> Arc<Self> {
        let node = Arc::new(Self::default());
        node.always(Method::GetTransactionCount, Answer::ok(&quantity(0)));
        node
    }

    /// Queues one answer for `method`, behind anything already queued.
    pub(crate) fn on(&self, method: Method, answer: Answer) -> &Self {
        self.queued.lock().entry(method).or_default().push_back(answer);
        self
    }

    /// A standing answer for `method`, used whenever the queue is empty.
    pub(crate) fn always(&self, method: Method, answer: Answer) -> &Self {
        self.standing.lock().insert(method, answer);
        self
    }

    /// Scripts a quiet wallet's pass with the tip, a balance sheet and an empty
    /// log window.
    pub(crate) fn quiet(&self, balances: &[Option<u128>]) -> &Self {
        self.always(Method::BlockNumber, Answer::ok(TIP_HEX));
        self.always(Method::Call, Answer::ok(&aggregate(balances)));
        self.always(Method::GetLogs, Answer::ok("[]"));
        self
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().clone()
    }

    pub(crate) fn calls_to(&self, method: Method) -> Vec<Call> {
        self.calls().into_iter().filter(|call| call.method == method).collect()
    }

    pub(crate) fn forget_calls(&self) {
        self.calls.lock().clear();
    }

    /// Makes every answer take `delay` of tokio time, for the budget cases.
    pub(crate) fn slow_by(&self, delay: Duration) -> &Self {
        *self.delay.lock() = Some(delay);
        self
    }

    async fn wait(&self) {
        let delay = *self.delay.lock();
        if let Some(delay) = delay {
            sleep(delay).await;
        }
    }

    fn take(&self, method: Method) -> Answer {
        let queued = self.queued.lock().get_mut(&method).and_then(VecDeque::pop_front);
        queued.or_else(|| self.standing.lock().get(&method).cloned()).unwrap_or_else(|| {
            Answer::Error {
                code: -32601,
                message: format!("the fake node has no answer for {method}"),
            }
        })
    }

    fn answer(&self, call: &RpcCall) -> Result<Box<RawValue>, EvmError> {
        self.calls
            .lock()
            .push(Call { method: call.method(), params: call.params().get().to_owned() });
        match self.take(call.method()) {
            Answer::Result(json) => Ok(raw(&json)),
            Answer::Error { code, message } => Err(EvmError::from_net(
                call.method(),
                NetError::Rpc { endpoint: endpoint(), code, message },
            )),
            Answer::Unreachable => Err(EvmError::from_net(
                call.method(),
                NetError::NoEndpoints { chain: ChainId::Ethereum },
            )),
        }
    }
}

#[expect(clippy::expect_used, reason = "test helper")]
fn raw(json: &str) -> Box<RawValue> {
    RawValue::from_string(json.to_owned()).expect("a scripted answer is json")
}

#[async_trait]
impl EvmNode for FakeNode {
    async fn call(&self, call: &RpcCall) -> Result<Answered, EvmError> {
        self.wait().await;
        Ok(Answered::new(endpoint(), vec![self.answer(call)?]))
    }

    async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, EvmError> {
        self.wait().await;
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(self.answer(call)?);
        }
        Ok(Answered::new(endpoint(), results))
    }

    async fn broadcast(&self, call: &RpcCall) -> Result<Answered, EvmError> {
        self.wait().await;
        Ok(Answered::new(endpoint(), vec![self.answer(call)?]))
    }
}

#[derive(Debug)]
pub(crate) struct Setup {
    pub(crate) params: &'static EvmParams,
    pub(crate) config: EvmConfig,
    pub(crate) history: Option<Arc<FakeHistory>>,
}

impl Default for Setup {
    fn default() -> Self {
        Self::on(ETHEREUM)
    }
}

impl Setup {
    pub(crate) fn on(params: &'static EvmParams) -> Self {
        Self { params, config: EvmConfig::default(), history: None }
    }

    pub(crate) fn with_history(history: Arc<FakeHistory>) -> Self {
        Self { history: Some(history), ..Self::default() }
    }

    /// One log window per pass, which keeps a scripted call count fixed.
    #[must_use]
    pub(crate) const fn one_window(mut self) -> Self {
        self.config.max_forward_windows = 1;
        self.config.max_backfill_windows = 0;
        self
    }
}

#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) adapter: EvmAdapter,
    pub(crate) node: Arc<FakeNode>,
    pub(crate) history: Option<Arc<FakeHistory>>,
    pub(crate) cursors: Arc<MemoryCursorStore>,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) events: FakeEventSink,
}

impl Harness {
    pub(crate) fn new(setup: Setup) -> Fallible<Self> {
        let clock = Arc::new(FakeClock::default());
        let as_clock: Arc<dyn Clock> = Arc::<FakeClock>::clone(&clock);
        let node = FakeNode::new();
        // Every quote and build reads the fee market, so a test that is about
        // something else still gets the recorded one.
        node.always(Method::FeeHistory, Answer::ok(&fixture("fee_history.json", "result")?));
        let cursors = Arc::new(MemoryCursorStore::new());
        let mut builder = EvmAdapter::builder(
            setup.params,
            Arc::<FakeNode>::clone(&node),
            Arc::<MemoryCursorStore>::clone(&cursors),
            as_clock,
        )
        .config(setup.config);
        if let Some(history) = setup.history.clone() {
            let api: Arc<dyn HistoryApi> = history;
            builder = builder.history(api);
        }
        Ok(Self {
            adapter: builder.build()?,
            node,
            history: setup.history,
            cursors,
            clock,
            events: FakeEventSink::new(),
        })
    }

    pub(crate) fn ethereum() -> Fallible<Self> {
        Self::new(Setup::default())
    }

    pub(crate) fn chain(&self) -> ChainId {
        self.adapter.chain()
    }

    /// A deadline that never expires while the fake clock stands still.
    pub(crate) fn deadline(&self) -> Deadline {
        self.clock.deadline_in(Duration::from_secs(60))
    }

    pub(crate) async fn watch(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        self.adapter.set_watch_set(addresses).await
    }

    pub(crate) async fn watch_owner(&self) -> Result<(), ChainError> {
        self.watch(&[watched(7, self.chain(), OWNER)]).await
    }

    pub(crate) async fn sync(&self) -> Result<ChainSyncProgress, ChainError> {
        self.adapter.sync(self.deadline(), &self.events).await
    }

    pub(crate) async fn sync_expired(&self) -> Result<ChainSyncProgress, ChainError> {
        self.adapter.sync(Deadline::EXPIRED, &self.events).await
    }

    pub(crate) fn drain(&self) -> Vec<WalletEvent> {
        self.events.drain()
    }

    /// A wallet watching the address the test key owns, whose node counts
    /// `nonce` sends from it and a balance of five coins.
    pub(crate) async fn signer(setup: Setup, nonce: u64) -> Fallible<Self> {
        let harness = Self::new(setup)?;
        harness.watch(&[watched(7, ChainId::Ethereum, SIGNER)]).await?;
        harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(u128::from(nonce))));
        harness.node.always(Method::GetBalance, Answer::ok(&quantity(5 * ETH)));
        Ok(harness)
    }

    /// A wallet whose one address has sent one transaction, already broadcast,
    /// with the calls that set it up forgotten.
    pub(crate) async fn sent() -> Fallible<Self> {
        Self::sent_with(Setup::default().one_window()).await
    }

    pub(crate) async fn sent_with(setup: Setup) -> Fallible<Self> {
        let harness = Self::signer(setup, 12).await?;
        harness.send(&payment(PEER, 12)).await?;
        harness.node.forget_calls();
        harness.drain();
        Ok(harness)
    }

    /// Builds `request`, signs it with the test key and broadcasts it to a node
    /// that echoes its txid.
    pub(crate) async fn send(&self, request: &TxRequest) -> Fallible<SignedTx> {
        let unsigned = self.adapter.build_tx(request).await?;
        let signed = self.adapter.sign_tx(&unsigned, &[key_material(TEST_KEY)?])?;
        self.node
            .on(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));
        self.adapter.broadcast(&signed).await?;
        Ok(signed)
    }

    /// The txids of the sends the fixture address's stored set holds, in order.
    pub(crate) async fn stored_txids(&self) -> Fallible<Vec<String>> {
        let stored = self
            .cursors
            .load_reservation(CursorScope::Address(7), ReservationSlot::EvmNonces)
            .await?
            .ok_or("a stored set")?;
        let set: serde_json::Value = stored.with_bytes(|bytes| serde_json::from_slice(bytes))?;
        Ok(set
            .get("entries")
            .and_then(serde_json::Value::as_array)
            .ok_or("stored entries")?
            .iter()
            .filter_map(|entry| entry["txid"].as_str().map(str::to_owned))
            .collect())
    }

    /// Drains the events and returns the transaction records among them.
    pub(crate) fn records(&self) -> Vec<TxRecord> {
        self.events.records()
    }
}

/// One preview or quote detail row, by label.
pub(crate) fn detail(details: &[TxDetail], label: &str) -> String {
    details
        .iter()
        .find(|row| row.label == label)
        .map_or_else(|| format!("(no detail row {label})"), |row| row.value.clone())
}

pub(crate) const fn usdt() -> AssetId {
    AssetId::Token(TokenId::UsdtErc20)
}

/// A real HTTP pool for construction checks. Building a pool opens no socket, so
/// nothing is ever identified.
pub(crate) fn http_pool(chain: ChainId, urls: &[&str]) -> Fallible<Arc<Pool<HttpEndpoint>>> {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let http = HttpClient::new(None)?;
    let runtime = NetRuntime::with_pacing(clock, PacerPolicy::UNTHROTTLED);
    let connector = HttpConnector::new(http, Arc::new(NoIdentity));
    let mut endpoints = Vec::with_capacity(urls.len());
    for url in urls {
        endpoints.push(Endpoint::parse(chain, Transport::HttpJsonrpc, url)?);
    }
    Ok(Arc::new(runtime.pool(chain, Transport::HttpJsonrpc, Arc::new(connector), endpoints)))
}
