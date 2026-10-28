//! A scripted TRON node and the harness that runs the shipped adapter over it.
//!
//! The fake implements [`TronNode`], so the decoders, request encoders, history
//! walk, and pending ledger all run as shipped.
//!
//! A script matches a [`Route`] and, optionally, a needle the request must
//! contain. The longest matching needle wins. A script with several queued
//! answers hands them out in order, then repeats the last one.

use std::collections::VecDeque;
use std::error::Error;
use std::fs;
use std::io;
use std::iter::once;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainId, ChainSyncProgress, Clock as _, Deadline,
    FailureKind, FeeSelection, KeyKind, KeyMaterial, SendAmount, TokenId, TxDetail, TxPreview,
    TxRequest, TxWarningKind, UnsignedTx, WalletEvent, WatchedAddress,
    test_util::{
        FakeClock, FakeEventSink, MemoryCursorStore, WatchedAddressBuilder, key_material_of,
    },
};
use jelly_net::NetError;
use jelly_tron::{
    Answered, Request, Route, TronAdapter, TronConfig, TronError, TronNode,
    test_util::{OWNER, RECIPIENT, TRX_OWNER, account_resource, endpoint},
};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::Value;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error>>;

/// A recipient the tests script with no account row.
pub(crate) const FRESH: &str = "TEpPGH6bFVgD7J4tnGfFqAstxTS3e6bPiF";
/// The account `token_page1.json` was recorded for.
pub(crate) const TOKEN_HISTORY: &str = "TBdQCi6AvGhCNk17Pmhqx9eoXMRFqEBiqo";

/// The length of the `0x41 || tail` body TRON's protobuf and HTTP API carry.
pub(crate) const BODY_BYTES: usize = 21;

/// The canonical spelling of an address a fixture carries as hex, encoded by `bs58`,
/// not by the code under test.
pub(crate) fn canonical(value: &str) -> Fallible<String> {
    let hex = value.strip_prefix("0x").unwrap_or(value);
    let body = <[u8; BODY_BYTES]>::try_from(hex::decode(hex)?.as_slice())?;
    Ok(bs58::encode(body).with_check().into_string())
}

/// The body of a `T...` address, decoded by `bs58`, not by the code under test.
pub(crate) fn body_of(address: &str) -> Fallible<[u8; BODY_BYTES]> {
    let decoded = bs58::decode(address).with_check(None).into_vec()?;
    Ok(<[u8; BODY_BYTES]>::try_from(decoded.as_slice())?)
}

/// The recipient body and amount of `transfer(address,uint256)` calldata.
///
/// Both are read by hand because the crate's decoder is under test. The
/// recipient word holds the twenty-byte tail, and the body puts TRON's prefix
/// byte back in front.
pub(crate) fn transfer_arguments(calldata: &[u8]) -> Fallible<([u8; BODY_BYTES], u128)> {
    let tail = calldata.get(16..36).ok_or("the calldata has no recipient word")?;
    let mut body = [0u8; BODY_BYTES];
    for (slot, byte) in body.iter_mut().zip(once(0x41).chain(tail.iter().copied())) {
        *slot = byte;
    }
    let low = calldata.get(52..68).ok_or("the calldata has no amount word")?;
    Ok((body, u128::from_be_bytes(low.try_into()?)))
}

pub(crate) const TRX: AssetId = AssetId::Native(ChainId::Tron);
pub(crate) const USDT_ASSET: AssetId = AssetId::Token(TokenId::UsdtTrc20);

/// How long the adapter leaves a quiet address's history unread.
pub(crate) const HISTORY_SWEEP: Duration = Duration::from_secs(600);

/// A recorded answer from `tests/fixtures`, read in when the suite compiles.
macro_rules! fixture {
    ($name:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/", $name, ".json"))
    };
}
pub(crate) use fixture;

pub(crate) fn vector(name: &str) -> Fallible<Value> {
    let path = format!("{}/tests/vectors/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = fs::read_to_string(&path).map_err(|error| format!("reading {path}: {error}"))?;
    Ok(serde_json::from_str(&text).map_err(|error| format!("parsing {path}: {error}"))?)
}

pub(crate) fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(ToOwned::to_owned)
}

pub(crate) fn number(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(Value::as_i64)
}

/// A transaction as an endpoint reported it, for a rebuild to be compared against.
#[derive(Debug, Deserialize)]
pub(crate) struct Recorded {
    pub(crate) raw_data_hex: String,
    #[serde(rename = "txID")]
    pub(crate) txid: String,
    pub(crate) raw_data: RecordedRaw,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecordedRaw {
    pub(crate) expiration: i64,
    pub(crate) timestamp: i64,
    #[serde(default)]
    pub(crate) fee_limit: i64,
    pub(crate) contract: [RecordedContract; 1],
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecordedContract {
    pub(crate) parameter: RecordedParameter,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecordedParameter {
    pub(crate) value: RecordedValue,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecordedValue {
    pub(crate) owner_address: String,
    #[serde(default)]
    pub(crate) to_address: Option<String>,
    #[serde(default)]
    pub(crate) amount: Option<u64>,
    #[serde(default)]
    pub(crate) data: Option<String>,
}

pub(crate) fn recorded(fixture: &str) -> serde_json::Result<Recorded> {
    serde_json::from_str(fixture)
}

impl Recorded {
    pub(crate) const fn value(&self) -> &RecordedValue {
        let [contract] = &self.raw_data.contract;
        &contract.parameter.value
    }

    /// The window this transaction was stamped with, so a rebuild against the same
    /// head block reproduces its bytes exactly.
    pub(crate) fn window(&self) -> Fallible<Duration> {
        let millis = self.raw_data.expiration.saturating_sub(self.raw_data.timestamp);
        Ok(Duration::from_millis(u64::try_from(millis)?))
    }
}

pub(crate) fn key_material(secret: [u8; 32]) -> KeyMaterial {
    key_material_of(KeyKind::Secp256k1, secret.to_vec())
}

pub(crate) fn watched(id: i64, address: &str) -> WatchedAddress {
    WatchedAddressBuilder::new(id, ChainId::Tron, address).build()
}

pub(crate) fn request(asset: AssetId, to: &str, amount: u128) -> TxRequest {
    TxRequest {
        from_address_ids: vec![1],
        to: to.to_owned(),
        destination_tag: None,
        comment: None,
        should_delete_account: false,
        asset,
        amount: SendAmount::Exact { value: Amount::from_base_units(amount) },
        fee: FeeSelection::NORMAL,
        nonce: None,
        data: None,
    }
}

pub(crate) fn send_max(asset: AssetId, to: &str) -> TxRequest {
    TxRequest { amount: SendAmount::Max, ..request(asset, to, 0) }
}

#[derive(Debug, Clone)]
pub(crate) enum Answer {
    Body(String),
    /// A completed exchange with a non-success status, as from a gateway that
    /// doesn't serve the route.
    Status(u16),
    /// The exchange never completed, so the outcome is unknown.
    Transport,
}

impl Answer {
    pub(crate) fn body(text: &str) -> Self {
        Self::Body(text.to_owned())
    }

    /// What the shipped node would return for this answer on `route`.
    fn reply(self, route: Route) -> Result<Answered, TronError> {
        match self {
            Self::Body(text) => Ok(Answered::new(endpoint(), route, text)),
            Self::Status(status) => {
                Err(TronError::Net(NetError::Status { endpoint: endpoint(), status }))
            }
            Self::Transport => Err(TronError::Net(NetError::endpoint(
                &endpoint(),
                FailureKind::Connect,
                io::Error::other("refused"),
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) route: Route,
    pub(crate) path: String,
    pub(crate) query: String,
    pub(crate) body: Option<String>,
}

#[derive(Debug)]
struct Script {
    route: Route,
    needle: String,
    queue: VecDeque<Answer>,
}

#[derive(Debug, Default)]
struct State {
    scripts: Vec<Script>,
    calls: Vec<Call>,
}

#[derive(Debug, Default)]
pub(crate) struct FakeNode {
    state: Mutex<State>,
}

impl FakeNode {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Scripts one answer for a route, whatever the request says.
    pub(crate) fn on(self: &Arc<Self>, route: Route, answer: Answer) -> Arc<Self> {
        self.queue(route, "", answer)
    }

    /// Scripts one answer for a route whose path, query, or body contains
    /// `needle`.
    pub(crate) fn on_matching(
        self: &Arc<Self>,
        route: Route,
        needle: &str,
        answer: Answer,
    ) -> Arc<Self> {
        self.queue(route, needle, answer)
    }

    /// Replaces every script for a route with one standing answer, so a test can
    /// override what the shared fixtures scripted.
    pub(crate) fn always(self: &Arc<Self>, route: Route, answer: Answer) -> Arc<Self> {
        let mut state = self.state.lock();
        state.scripts.retain(|script| script.route != route);
        state.scripts.push(Script {
            route,
            needle: String::new(),
            queue: VecDeque::from([answer]),
        });
        drop(state);
        Arc::clone(self)
    }

    fn queue(self: &Arc<Self>, route: Route, needle: &str, answer: Answer) -> Arc<Self> {
        let mut state = self.state.lock();
        match state
            .scripts
            .iter_mut()
            .find(|script| script.route == route && script.needle == needle)
        {
            Some(script) => script.queue.push_back(answer),
            None => state.scripts.push(Script {
                route,
                needle: needle.to_owned(),
                queue: VecDeque::from([answer]),
            }),
        }
        drop(state);
        Arc::clone(self)
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.state.lock().calls.clone()
    }

    pub(crate) fn count(&self, route: Route) -> usize {
        self.calls().iter().filter(|call| call.route == route).count()
    }

    pub(crate) fn last_body(&self, route: Route) -> Option<String> {
        let call = self.calls().into_iter().rfind(|call| call.route == route)?;
        call.body
    }

    pub(crate) fn forget_calls(&self) {
        self.state.lock().calls.clear();
    }

    /// Records the call and hands out the answer scripted for it. An unscripted
    /// request panics, since the test is missing a script.
    #[expect(clippy::panic, reason = "an error here could pass for the failure a test expects")]
    fn dispatch(&self, request: &Request) -> Answer {
        let route = request.route();
        let haystack = format!(
            "{} {} {}",
            request.path(),
            request.query(),
            request.body().unwrap_or_default()
        );
        let mut state = self.state.lock();
        state.calls.push(Call {
            route,
            path: request.path().to_owned(),
            query: request.query().to_owned(),
            body: request.body().map(ToOwned::to_owned),
        });
        let script = state
            .scripts
            .iter_mut()
            .filter(|script| script.route == route && haystack.contains(&script.needle))
            .max_by_key(|script| script.needle.len())
            .unwrap_or_else(|| panic!("no script for {route} at {}", request.path()));
        if script.queue.len() > 1 {
            script.queue.pop_front().unwrap_or(Answer::Transport)
        } else {
            script.queue.front().cloned().unwrap_or(Answer::Transport)
        }
    }
}

#[async_trait]
impl TronNode for FakeNode {
    async fn call(&self, request: &Request) -> Result<Answered, TronError> {
        self.dispatch(request).reply(request.route())
    }

    /// Mirrors the shipped node, which makes one attempt on one endpoint and falls
    /// back to the JSON route when the hex route answers 404 or 405.
    async fn broadcast(
        &self,
        preferred: &Request,
        fallback: &Request,
    ) -> Result<Answered, TronError> {
        match self.dispatch(preferred).reply(preferred.route()) {
            Err(TronError::Net(NetError::Status { status: 404 | 405, .. })) => {
                self.dispatch(fallback).reply(fallback.route())
            }
            other => other,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) fake: Arc<FakeNode>,
    pub(crate) cursors: Arc<MemoryCursorStore>,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) adapter: TronAdapter,
}

impl Harness {
    pub(crate) fn new(fake: &Arc<FakeNode>) -> Self {
        Self::with(fake, TronConfig::default())
    }

    pub(crate) fn with(fake: &Arc<FakeNode>, config: TronConfig) -> Self {
        let clock = Arc::new(FakeClock::default());
        let cursors = Arc::new(MemoryCursorStore::new());
        let adapter = TronAdapter::new(
            Arc::<FakeNode>::clone(fake),
            Arc::<MemoryCursorStore>::clone(&cursors),
            Arc::<FakeClock>::clone(&clock),
        )
        .config(config);
        Self { fake: Arc::clone(fake), cursors, clock, adapter }
    }

    pub(crate) fn deadline(&self) -> Deadline {
        self.clock.deadline_in(Duration::from_secs(30))
    }
}

/// A harness whose watch set is [`OWNER`] as address 1.
pub(crate) async fn ready(fake: &Arc<FakeNode>) -> Fallible<Harness> {
    let harness = Harness::new(fake);
    harness.adapter.set_watch_set(&[watched(1, OWNER)]).await?;
    Ok(harness)
}

/// A harness whose watch set is [`TRX_OWNER`] as address 4.
pub(crate) async fn watching_owner(fake: &Arc<FakeNode>, config: TronConfig) -> Fallible<Harness> {
    let harness = Harness::with(fake, config);
    harness.adapter.set_watch_set(&[watched(4, TRX_OWNER)]).await?;
    Ok(harness)
}

pub(crate) fn account(address: &str, balance_sun: u64) -> Answer {
    Answer::Body(format!(
        r#"{{"address":"{address}","balance":{balance_sun},"create_time":1542274980000}}"#
    ))
}

/// What `getaccountresource` answers for an account with the given pools.
pub(crate) fn resources(free_used: u64, staked_limit: u64, energy_limit: u64) -> Answer {
    Answer::Body(account_resource(free_used, staked_limit, energy_limit))
}

/// Scripts the head block, governance parameters, and sender's pools that every
/// build and quote reads.
pub(crate) fn base(fake: &Arc<FakeNode>) -> Arc<FakeNode> {
    fake.on(Route::NowBlock, Answer::body(fixture!("now_block")))
        .on(Route::ChainParameters, Answer::body(fixture!("chain_parameters")))
        .on(Route::AccountResource, Answer::body(fixture!("account_resource")))
}

/// A wallet that can pay. The owner holds `balance` sun, [`RECIPIENT`] has an
/// account row, and both TRC-20 constant calls answer.
pub(crate) fn funded(balance: u64) -> Arc<FakeNode> {
    let fake = FakeNode::new();
    base(&fake)
        .on_matching(Route::Account, OWNER, account(OWNER, balance))
        .on_matching(Route::Account, RECIPIENT, account(RECIPIENT, 5_000_000))
        .on_matching(
            Route::ConstantContract,
            "balanceOf(address)",
            Answer::body(fixture!("constant_balance_of")),
        )
        .on_matching(
            Route::ConstantContract,
            "transfer(address,uint256)",
            Answer::body(fixture!("constant_transfer")),
        )
}

/// A wallet that can send, with every history stream quiet so a pass is only
/// about the follow-up.
pub(crate) fn sender() -> Arc<FakeNode> {
    let fake = FakeNode::new();
    let fake = base(&fake)
        .on_matching(Route::Account, OWNER, account(OWNER, 100_000_000))
        .on_matching(Route::Account, RECIPIENT, account(RECIPIENT, 5_000_000))
        .on_matching(
            Route::ConstantContract,
            "balanceOf(address)",
            Answer::body(fixture!("constant_balance_of")),
        )
        .on(Route::BroadcastHex, Answer::body(r#"{"result":true}"#));
    quiet_history(&fake)
}

/// An empty page on every history stream, as for a wallet with no history.
pub(crate) fn quiet_history(fake: &Arc<FakeNode>) -> Arc<FakeNode> {
    fake.on(Route::NativeHistory, Answer::body(fixture!("page_empty")))
        .on(Route::TokenHistory, Answer::body(fixture!("page_empty")))
        .on(Route::InternalHistory, Answer::body(fixture!("page_empty")))
}

/// A wallet holding 7 TRX whose token balances answer and that no contract has
/// paid, so a test scripts only the native and TRC-20 history it's about.
pub(crate) fn balances_only(address: &str) -> Arc<FakeNode> {
    let fake = FakeNode::new();
    base(&fake)
        .on_matching(Route::Account, address, account(address, 7_000_000))
        .on_matching(
            Route::ConstantContract,
            "balanceOf(address)",
            Answer::body(fixture!("constant_balance_of")),
        )
        .on(Route::InternalHistory, Answer::body(fixture!("page_empty")))
}

pub(crate) async fn pass(harness: &Harness, sink: &FakeEventSink) -> Fallible<ChainSyncProgress> {
    Ok(harness.adapter.sync(harness.deadline(), sink).await?)
}

/// One `/v1` history page of the given rows, with a fingerprint when a further
/// page might exist.
pub(crate) fn history_page(rows: &[&str], fingerprint: Option<&str>) -> Answer {
    let next =
        fingerprint.map_or_else(String::new, |cursor| format!(r#""fingerprint":"{cursor}""#));
    Answer::body(&format!(r#"{{"data":[{}],"meta":{{{next}}}}}"#, rows.join(",")))
}

/// A TRX transfer as the native stream lists it, mined in block 85304900 with no
/// fee.
pub(crate) fn transfer_row(
    txid: &str,
    timestamp_millis: i64,
    from_hex: &str,
    to_hex: &str,
    sun: u64,
) -> String {
    format!(
        r#"{{"txID":"{txid}","blockNumber":85304900,"block_timestamp":{timestamp_millis},
            "ret":[{{"contractRet":"SUCCESS","fee":0}}],
            "raw_data":{{"contract":[{{"type":"TransferContract","parameter":{{"value":{{
              "amount":{sun},"owner_address":"{from_hex}","to_address":"{to_hex}"}}}}}}]}}}}"#
    )
}

pub(crate) fn balances(events: &[WalletEvent]) -> Vec<(i64, AssetId, Amount)> {
    events
        .iter()
        .filter_map(|event| match *event {
            WalletEvent::BalanceChanged(ref update) => {
                Some((update.address_id, update.asset, update.balance))
            }
            WalletEvent::TxDetected(_)
            | WalletEvent::TxUpdated(_)
            | WalletEvent::TxConfirmed(_)
            | WalletEvent::SyncProgress(_) => None,
        })
        .collect()
}

pub(crate) fn detail<'a>(rows: &'a [TxDetail], label: &str) -> Option<&'a str> {
    rows.iter().find(|row| row.label == label).map(|row| row.value.as_str())
}

/// The `raw_data` a build carries, as hex. The payload is the seven-byte
/// `tronraw` tag in front of the bytes that are signed.
pub(crate) fn raw_data_hex(unsigned: &UnsignedTx) -> Option<String> {
    unsigned.payload.as_bytes().strip_prefix(b"tronraw").map(hex::encode)
}

pub(crate) fn has_detail(rows: &[TxDetail], label: &str) -> bool {
    rows.iter().any(|row| row.label == label)
}

pub(crate) fn warned(preview: &TxPreview, kind: TxWarningKind) -> bool {
    preview.warnings.iter().any(|warning| warning.kind == kind)
}
