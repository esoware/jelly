//! Fixtures every suite in this crate shares.
//!
//! Everything goes through the shipped [`SolanaAdapter`] over a scripted node, so
//! a test exercises the same JSON-RPC documents, decoders, signature walk and
//! pending ledger a socket would.

use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, ChainSyncProgress, Clock as _,
    Deadline, FeeSelection, KeyKind, KeyMaterial, SignedTx, TokenId, TxDetail, TxRecord, TxRequest,
    TxWarning, TxWarningKind, WalletEvent, WatchedAddress,
    test_util::{
        FakeClock, FakeEventSink, MemoryCursorStore, TxRequestBuilder, WatchedAddressBuilder,
        key_material,
    },
};
use jelly_net::NetError;
use jelly_solana::{
    Answered, BroadcastReply, Method, RpcCall, SolanaAdapter, SolanaConfig, SolanaError,
    SolanaNode,
    test_util::{TokenState, endpoint, token_account_data},
};
use parking_lot::Mutex;
use serde_json::value::RawValue;
use serde_json::{Value, json};
use solana_message::{VersionedMessage, v0};
use solana_pubkey::Pubkey;
use solana_transaction::versioned::VersionedTransaction;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error>>;

/// The address the fixture wallet watches. It is the public key of a seed of
/// repeated 0x07, so the same wallet can sign.
pub(crate) const OWNER: &str = "GmaDrppBC7P5ARKV8g3djiwP89vz1jLK23V2GBjuAEGB";
/// The seed behind [`OWNER`]. Test material only, never a wallet key.
pub(crate) const OWNER_SEED: u8 = 7;
/// An address outside the wallet.
pub(crate) const PEER: &str = "5tzFkiKscXHK5ZXCGbXZxdw7gTjjD1mBwuoFbhUvuAi9";
/// The address the recorded mainnet fixtures belong to, which also serves as the
/// second watched address in the multi-address cases.
pub(crate) const RECORDED: &str = "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM";
pub(crate) const USDC_MINT: &str = "EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v";
pub(crate) const BLOCKHASH: &str = "HezQLwwK3aecM7H2cRSqFhidamdAHUNXcUgiyP7FcTjA";

/// A program-derived address, so it is off the curve and not a wallet.
pub(crate) const DERIVED: &str = "FGETo8T8wMcN2wCjav8VK6eh3dLk63evNDPxzLSJra8B";

/// The published program addresses a transaction names, spelled here rather than
/// taken from the code under test.
pub(crate) const SYSTEM_PROGRAM: &str = "11111111111111111111111111111111";
pub(crate) const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub(crate) const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub(crate) const COMPUTE_BUDGET_PROGRAM: &str = "ComputeBudget111111111111111111111111111111";

/// The largest serialised transaction the cluster accepts.
pub(crate) const MAX_TRANSACTION_BYTES: usize = 1_232;

pub(crate) const SLOT: u64 = 438_929_791;
pub(crate) const BLOCK_HEIGHT: u64 = 416_000_000;
pub(crate) const EXPIRY_HEIGHT: u64 = BLOCK_HEIGHT + 150;
/// What every fixture wallet holds.
pub(crate) const BALANCE_LAMPORTS: u64 = 5_000_000_000;
pub(crate) const BASE_FEE_LAMPORTS: u64 = 5_000;

/// Every token Jelly carries on Solana, in the order the adapter reports them.
pub(crate) const SOLANA_TOKEN_IDS: [TokenId; 12] = [
    TokenId::LinkSpl,
    TokenId::WbtcSpl,
    TokenId::EnaSpl,
    TokenId::PumpSpl,
    TokenId::JupSpl,
    TokenId::UsdtSpl,
    TokenId::UsdcSpl,
    TokenId::UsdsSpl,
    TokenId::UsdgSpl,
    TokenId::PyusdSpl,
    TokenId::XautSpl,
    TokenId::PaxgSpl,
];

/// The accounts one owner's balances are read from, its own and one per token.
pub(crate) const ACCOUNTS_PER_OWNER: usize = SOLANA_TOKEN_IDS.len().saturating_add(1);

/// The recorded mainnet answers, compiled in so a test never touches the
/// filesystem.
pub(crate) const BALANCE_BODY: &str = include_str!("../../fixtures/get_balance.json");
pub(crate) const RECENT_FEES: &str = include_str!("../../fixtures/recent_prioritization_fees.json");
/// [`RECORDED`]'s associated USDC account, read by address.
pub(crate) const TOKEN_ACCOUNT_USDC: &str = include_str!("../../fixtures/token_account_usdc.json");
pub(crate) const TX_NATIVE_TRANSFER: &str = include_str!("../../fixtures/tx_native_transfer.json");
pub(crate) const TX_SPL_TRANSFER: &str = include_str!("../../fixtures/tx_spl_transfer.json");
pub(crate) const TX_USDC_TRANSFER: &str = include_str!("../../fixtures/tx_usdc_transfer.json");
/// A USDT send to an owner that had never held USDT, so the sender also paid the
/// rent of the recipient's new token account. The untrimmed `result` of its
/// `getTransaction` answer.
pub(crate) const TX_USDT_TRANSFER_NEW_ACCOUNT: &str =
    include_str!("../../fixtures/tx_usdt_transfer_new_account.json");
/// Version 1 transactions, each the untrimmed `result` of its `getTransaction`
/// answer.
pub(crate) const TX_V1_NATIVE_TRANSFER: &str =
    include_str!("../../fixtures/tx_v1_native_transfer.json");
pub(crate) const TX_V1_USDC_TRANSFER: &str =
    include_str!("../../fixtures/tx_v1_usdc_transfer.json");
pub(crate) const TX_V1_FAILED: &str = include_str!("../../fixtures/tx_v1_failed.json");

/// The pinned instruction layouts and derivations.
pub(crate) const VECTORS: &str = include_str!("../../vectors/instructions.json");

#[derive(Debug, Clone)]
pub(crate) enum Answer {
    /// The JSON `result` member, verbatim.
    Result(String),
    /// A JSON-RPC error object.
    Error { code: i64, message: String, data: Option<String> },
    /// No endpoint answered at all.
    Unreachable,
}

impl Answer {
    pub(crate) fn ok(value: &Value) -> Self {
        Self::Result(value.to_string())
    }

    pub(crate) fn raw(json: &str) -> Self {
        Self::Result(json.to_owned())
    }

    pub(crate) fn refused(code: i64, message: &str) -> Self {
        Self::Error { code, message: message.to_owned(), data: None }
    }

    /// A preflight refusal carrying the runtime's own structured error.
    pub(crate) fn preflight(error: &Value, logs: &[&str]) -> Self {
        Self::Error {
            code: -32002,
            message: String::from("Transaction simulation failed"),
            data: Some(json!({ "err": error, "logs": logs }).to_string()),
        }
    }

    fn body(&self) -> String {
        match *self {
            Self::Result(ref json) => format!(r#"{{"jsonrpc":"2.0","id":1,"result":{json}}}"#),
            Self::Error { code, ref message, ref data } => {
                let mut error = json!({ "code": code, "message": message });
                if let Some(raw) = data.as_deref() {
                    let parsed: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
                    if let Some(object) = error.as_object_mut() {
                        object.insert(String::from("data"), parsed);
                    }
                }
                json!({ "jsonrpc": "2.0", "id": 1, "error": error }).to_string()
            }
            Self::Unreachable => String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Call {
    pub(crate) method: Method,
    pub(crate) params: String,
}

impl Call {
    /// The first positional parameter, as text.
    pub(crate) fn first(&self) -> Option<String> {
        let params: Value = serde_json::from_str(&self.params).ok()?;
        params.get(0)?.as_str().map(str::to_owned)
    }

    pub(crate) fn contains(&self, needle: &str) -> bool {
        self.params.contains(needle)
    }
}

/// A ledger of signatures for one address, newest first.
#[derive(Debug, Default, Clone)]
struct Ledger {
    rows: Vec<(String, u64)>,
}

impl Ledger {
    fn seed(&mut self, count: usize, prefix: &str) {
        let top = self.rows.first().map_or(0, |(_, slot)| *slot);
        let mut fresh: Vec<(String, u64)> = (0..count)
            .map(|index| {
                let step = u64::try_from(count.saturating_sub(index)).unwrap_or(1);
                (format!("{prefix}{index:05}"), top.saturating_add(step))
            })
            .collect();
        fresh.append(&mut self.rows);
        self.rows = fresh;
    }

    fn page(&self, params: &Value) -> Result<Value, SolanaError> {
        let config = params.get(1);
        let limit = config
            .and_then(|entry| entry.get("limit"))
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
            .unwrap_or(1);
        let before = config.and_then(|entry| entry.get("before")).and_then(Value::as_str);
        let until = config.and_then(|entry| entry.get("until")).and_then(Value::as_str);
        for signature in before.into_iter().chain(until) {
            if !self.rows.iter().any(|(known, _)| known == signature) {
                return Err(SolanaError::Rpc {
                    endpoint: endpoint(),
                    method: Method::GetSignaturesForAddress,
                    code: -32020,
                    message: format!("Transaction {signature} not found"),
                });
            }
        }
        let start = match before {
            None => 0,
            Some(marker) => self
                .rows
                .iter()
                .position(|(signature, _)| signature == marker)
                .map_or(0, |at| at.saturating_add(1)),
        };
        let mut entries = Vec::new();
        for (signature, slot) in self.rows.iter().skip(start) {
            if Some(signature.as_str()) == until || entries.len() >= limit {
                break;
            }
            entries.push(
                json!({ "signature": signature, "slot": slot, "blockTime": 1_700_000_000i64 }),
            );
        }
        Ok(Value::Array(entries))
    }
}

/// A node whose every answer is scripted.
///
/// An answer keyed to an account or signature is taken first, then the standing
/// one. A refusal goes through the shipped conversion, so a test sees the typed
/// failure a real node's wording produces. A transaction newer than its
/// request's version cap is refused the way a node refuses it.
#[derive(Debug, Default)]
pub(crate) struct FakeNode {
    keyed: Mutex<HashMap<(Method, String), Answer>>,
    standing: Mutex<HashMap<Method, Answer>>,
    ledgers: Mutex<HashMap<String, Ledger>>,
    calls: Mutex<Vec<Call>>,
    documents: Mutex<Vec<usize>>,
}

impl FakeNode {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// The answers a pass over a quiet wallet of the fixture addresses needs, none
    /// of which has a token account yet.
    pub(crate) fn quiet(&self) -> Fallible<&Self> {
        self.always(Method::GetSlot, Answer::ok(&json!(SLOT)));
        self.always(Method::GetBlockHeight, Answer::ok(&json!(BLOCK_HEIGHT)));
        self.always(Method::GetBalance, Answer::ok(&contextual(&json!(BALANCE_LAMPORTS))));
        self.always(Method::GetSignaturesForAddress, Answer::ok(&json!([])));
        for owner in [OWNER, RECORDED, PEER] {
            for token in SOLANA_TOKEN_IDS {
                let account = token_account_address(owner, token)?;
                self.at(Method::GetAccountInfo, &account, Answer::ok(&missing_account()));
            }
        }
        Ok(self)
    }

    /// Everything a build or a quote reads, on top of [`FakeNode::quiet`].
    pub(crate) fn priced(&self) -> Fallible<&Self> {
        self.quiet()?;
        self.always(
            Method::GetLatestBlockhash,
            Answer::ok(&contextual(&json!({
                "blockhash": BLOCKHASH,
                "lastValidBlockHeight": EXPIRY_HEIGHT,
            }))),
        );
        self.always(Method::GetAccountInfo, Answer::ok(&system_account()));
        self.always(Method::GetMinimumBalanceForRentExemption, Answer::ok(&json!(890_880u64)));
        self.always(Method::GetRecentPrioritizationFees, Answer::raw(RECENT_FEES));
        self.always(Method::SimulateTransaction, Answer::ok(&simulation(450)));
        Ok(self)
    }

    /// A standing answer for `method`, used whenever nothing more specific fits.
    pub(crate) fn always(&self, method: Method, answer: Answer) -> &Self {
        self.standing.lock().insert(method, answer);
        self
    }

    /// A standing answer for one account or signature.
    pub(crate) fn at(&self, method: Method, key: &str, answer: Answer) -> &Self {
        self.keyed.lock().insert((method, key.to_owned()), answer);
        self
    }

    /// Seeds `count` signatures for an address, newest first.
    pub(crate) fn seed_signatures(&self, address: &str, count: usize) -> &Self {
        self.ledgers.lock().entry(address.to_owned()).or_default().seed(count, "sig");
        self
    }

    /// Adds `count` newer signatures in front of an address's ledger.
    pub(crate) fn prepend_signatures(&self, address: &str, count: usize) -> &Self {
        self.ledgers.lock().entry(address.to_owned()).or_default().seed(count, "new");
        self
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.calls.lock().clone()
    }

    pub(crate) fn calls_to(&self, method: Method) -> Vec<Call> {
        self.calls().into_iter().filter(|call| call.method == method).collect()
    }

    pub(crate) fn count(&self, method: Method) -> usize {
        self.calls_to(method).len()
    }

    pub(crate) fn forget_calls(&self) {
        self.calls.lock().clear();
        self.documents.lock().clear();
    }

    /// How many calls each document carried, in order.
    pub(crate) fn documents(&self) -> Vec<usize> {
        self.documents.lock().clone()
    }

    fn selector(call: &RpcCall) -> Option<String> {
        let params: Value = serde_json::from_str(call.params().get()).ok()?;
        match params.get(0)? {
            Value::String(text) => Some(text.clone()),
            Value::Array(items) => Some(items.first()?.as_str()?.to_owned()),
            Value::Number(number) => Some(number.to_string()),
            Value::Null | Value::Bool(_) | Value::Object(_) => None,
        }
    }

    fn take(&self, call: &RpcCall) -> Answer {
        let method = call.method();
        if let Some(selector) = Self::selector(call) {
            let keyed = self.keyed.lock().get(&(method, selector)).cloned();
            if let Some(keyed) = keyed {
                return keyed;
            }
        }
        let standing = self.standing.lock().get(&method).cloned();
        standing.unwrap_or_else(|| {
            Answer::refused(-32601, &format!("the scripted node has no answer for {method}"))
        })
    }

    fn answer(&self, call: &RpcCall) -> Result<Box<RawValue>, SolanaError> {
        self.calls
            .lock()
            .push(Call { method: call.method(), params: call.params().get().to_owned() });

        if call.method() == Method::GetSignaturesForAddress
            && let Some(address) = Self::selector(call)
            && let Some(ledger) = self.ledgers.lock().get(&address).cloned()
        {
            let params: Value = serde_json::from_str(call.params().get()).unwrap_or(Value::Null);
            return raw(call.method(), &ledger.page(&params)?.to_string());
        }

        match self.take(call) {
            Answer::Result(json) => match Self::unsupported_version(call, &json) {
                None => raw(call.method(), &json),
                Some(version) => Err(SolanaError::from_net(
                    call.method(),
                    NetError::Rpc {
                        endpoint: endpoint(),
                        code: -32015,
                        message: format!(
                            "Transaction version ({version}) is not supported by the requesting \
                             client."
                        ),
                    },
                )),
            },
            Answer::Error { code, message, .. } => Err(SolanaError::from_net(
                call.method(),
                NetError::Rpc { endpoint: endpoint(), code, message },
            )),
            Answer::Unreachable => Err(SolanaError::from_net(
                call.method(),
                NetError::NoEndpoints { chain: ChainId::Solana },
            )),
        }
    }

    /// The version of a scripted transaction newer than the cap its request names,
    /// which a node refuses rather than answers. A request that names no cap is
    /// refused every versioned transaction.
    fn unsupported_version(call: &RpcCall, json: &str) -> Option<u64> {
        if call.method() != Method::GetTransaction {
            return None;
        }
        let version = serde_json::from_str::<Value>(json).ok()?.get("version")?.as_u64()?;
        let params: Value = serde_json::from_str(call.params().get()).ok()?;
        let cap = params
            .get(1)
            .and_then(|config| config.get("maxSupportedTransactionVersion"))
            .and_then(Value::as_u64);
        cap.is_none_or(|cap| version > cap).then_some(version)
    }
}

/// A scripted answer that is not JSON is refused the way a node refuses one it cannot
/// parse.
fn raw(method: Method, json: &str) -> Result<Box<RawValue>, SolanaError> {
    RawValue::from_string(json.to_owned()).map_err(|_| {
        SolanaError::from_net(
            method,
            NetError::Rpc {
                endpoint: endpoint(),
                code: -32700,
                message: String::from("the scripted answer is not json"),
            },
        )
    })
}

#[async_trait]
impl SolanaNode for FakeNode {
    async fn call(&self, call: &RpcCall) -> Result<Answered, SolanaError> {
        self.documents.lock().push(1);
        Ok(Answered::new(endpoint(), vec![self.answer(call)?]))
    }

    async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, SolanaError> {
        self.documents.lock().push(calls.len());
        let mut results = Vec::with_capacity(calls.len());
        for call in calls {
            results.push(self.answer(call)?);
        }
        Ok(Answered::new(endpoint(), results))
    }

    async fn broadcast(&self, call: &RpcCall) -> Result<BroadcastReply, SolanaError> {
        self.documents.lock().push(1);
        self.calls
            .lock()
            .push(Call { method: call.method(), params: call.params().get().to_owned() });
        let answer = self.take(call);
        if matches!(answer, Answer::Unreachable) {
            return Err(SolanaError::from_net(
                Method::SendTransaction,
                NetError::NoEndpoints { chain: ChainId::Solana },
            ));
        }
        BroadcastReply::read(&endpoint(), &answer.body())
    }
}

#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) adapter: SolanaAdapter,
    pub(crate) node: Arc<FakeNode>,
    pub(crate) cursors: Arc<MemoryCursorStore>,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) events: FakeEventSink,
}

impl Harness {
    pub(crate) fn with(config: SolanaConfig) -> Self {
        Self::over(config, Arc::new(MemoryCursorStore::new()))
    }

    pub(crate) fn over(config: SolanaConfig, cursors: Arc<MemoryCursorStore>) -> Self {
        let clock = Arc::new(FakeClock::default());
        let node = FakeNode::new();
        let adapter = SolanaAdapter::builder(
            Arc::<FakeNode>::clone(&node),
            Arc::<MemoryCursorStore>::clone(&cursors),
            Arc::<FakeClock>::clone(&clock),
        )
        .config(config)
        .build();
        Self { adapter, node, cursors, clock, events: FakeEventSink::new() }
    }

    pub(crate) fn new() -> Self {
        Self::with(SolanaConfig::default())
    }

    /// A harness whose node answers everything a build needs and whose one watched
    /// address is the fixture wallet at row seven.
    pub(crate) async fn priced() -> Fallible<Self> {
        let harness = Self::new();
        harness.node.priced()?;
        harness.watch_owner().await?;
        Ok(harness)
    }

    /// A deadline that never expires while the fake clock stands still.
    pub(crate) fn deadline(&self) -> Deadline {
        self.clock.deadline_in(Duration::from_secs(60))
    }

    pub(crate) async fn watch(&self, addresses: &[WatchedAddress]) -> Result<(), ChainError> {
        self.adapter.set_watch_set(addresses).await
    }

    pub(crate) async fn watch_owner(&self) -> Result<(), ChainError> {
        self.watch(&[watched(7, OWNER)]).await
    }

    pub(crate) async fn sync(&self) -> Result<ChainSyncProgress, ChainError> {
        self.adapter.sync(self.deadline(), &self.events).await
    }

    pub(crate) fn drain(&self) -> Vec<WalletEvent> {
        self.events.drain()
    }

    pub(crate) fn records(&self) -> Vec<TxRecord> {
        self.events.records()
    }

    pub(crate) fn balances(&self) -> Vec<(i64, AssetId, Amount)> {
        self.drain()
            .into_iter()
            .filter_map(|event| match event {
                WalletEvent::BalanceChanged(update) => {
                    Some((update.address_id, update.asset, update.balance))
                }
                WalletEvent::TxDetected(_)
                | WalletEvent::TxUpdated(_)
                | WalletEvent::TxConfirmed(_)
                | WalletEvent::SyncProgress(_) => None,
            })
            .collect()
    }
}

/// The associated token account of `owner` for `token`, derived with the SDK the
/// way the associated token program derives it, so a test never takes the
/// address from the code under test.
pub(crate) fn token_account_address(owner: &str, token: TokenId) -> Fallible<String> {
    let owner: Pubkey = owner.parse()?;
    let mint: Pubkey = token.contract().parse()?;
    let program =
        if is_token_2022(token) { spl_token_2022_interface::ID } else { TOKEN_PROGRAM.parse()? };
    let associated: Pubkey = ASSOCIATED_TOKEN_PROGRAM.parse()?;
    let (address, _bump) = Pubkey::try_find_program_address(
        &[owner.as_ref(), program.as_ref(), mint.as_ref()],
        &associated,
    )
    .ok_or("no bump seed derives the address")?;
    Ok(address.to_string())
}

/// True for the carried tokens whose mint the Token-2022 program owns.
pub(crate) const fn is_token_2022(token: TokenId) -> bool {
    matches!(token, TokenId::PumpSpl | TokenId::UsdgSpl | TokenId::PyusdSpl | TokenId::PaxgSpl)
}

/// The size of a token account of `token`. A Paxos mint's accounts also hold a
/// withheld transfer fee, and PUMP's only the hook state.
pub(crate) const fn account_bytes(token: TokenId) -> u64 {
    if matches!(token, TokenId::PumpSpl) {
        175
    } else if is_token_2022(token) {
        187
    } else {
        165
    }
}

pub(crate) fn watched(id: i64, address: &str) -> WatchedAddress {
    WatchedAddressBuilder::new(id, ChainId::Solana, address).derived("m/44'/501'/0'/0'").build()
}

/// The fixture wallet's signing key, an ed25519 seed of repeated 0x07.
pub(crate) fn owner_key() -> KeyMaterial {
    key_material(KeyKind::Ed25519Seed, OWNER_SEED)
}

pub(crate) fn other_key() -> KeyMaterial {
    key_material(KeyKind::Ed25519Seed, 9)
}

pub(crate) const fn native() -> AssetId {
    AssetId::Native(ChainId::Solana)
}

pub(crate) const fn usdc() -> AssetId {
    AssetId::Token(TokenId::UsdcSpl)
}

/// A send of `amount` base units to `to`, at the normal fee level.
pub(crate) fn request(to: &str, asset: AssetId, amount: u64) -> TxRequest {
    TxRequestBuilder::new(7, to, asset, Amount::from(amount)).build()
}

pub(crate) fn send_max(to: &str, asset: AssetId) -> TxRequest {
    TxRequestBuilder::new(7, to, asset, Amount::from_base_units(1)).send_max().build()
}

/// A send with its priority fee in lamports chosen by hand.
pub(crate) fn with_chosen_fee(
    to: &str,
    asset: AssetId,
    amount: u64,
    priority_lamports: u64,
) -> TxRequest {
    let mut request = request(to, asset, amount);
    request.fee = FeeSelection::Solana { priority_fee_lamports: priority_lamports.to_string() };
    request
}

/// The `context` plus `value` wrapper the state reads answer with.
pub(crate) fn contextual(value: &Value) -> Value {
    json!({ "context": { "slot": SLOT }, "value": value })
}

/// An account owned by the system program, which is what a wallet address is.
pub(crate) fn system_account() -> Value {
    contextual(&json!({
        "owner": SYSTEM_PROGRAM,
        "lamports": BALANCE_LAMPORTS,
        "data": ["", "base64"],
    }))
}

/// An account owned by the token program, which is what a token account is.
pub(crate) fn token_program_account() -> Fallible<Value> {
    token_account(RECORDED, USDC_MINT, 0)
}

pub(crate) fn missing_account() -> Value {
    contextual(&Value::Null)
}

pub(crate) fn simulation(units: u64) -> Value {
    contextual(&json!({ "err": Value::Null, "unitsConsumed": units, "logs": [] }))
}

/// A base64 `getAccountInfo` answer for an open token account that `owner`
/// holds `amount` of `mint` in.
pub(crate) fn token_account(owner: &str, mint: &str, amount: u64) -> Fallible<Value> {
    token_account_in(owner, mint, amount, TokenState::default())
}

/// A token account packed with the SDK the way its program stores it.
///
/// The reader under test then unpacks what a node would serve. A Token-2022
/// account carries the immutable owner every associated account has.
pub(crate) fn token_account_in(
    owner: &str,
    mint: &str,
    amount: u64,
    state: TokenState,
) -> Fallible<Value> {
    let token = SOLANA_TOKEN_IDS.into_iter().find(|token| token.contract() == mint);
    let program: Pubkey = if token.is_some_and(is_token_2022) {
        spl_token_2022_interface::ID
    } else {
        TOKEN_PROGRAM.parse()?
    };
    let data = token_account_data(&program, &mint.parse()?, &owner.parse()?, amount, state)
        .ok_or("the SDK refused to pack the token account")?;
    Ok(contextual(&json!({
        "lamports": 2_039_280u64,
        "data": [BASE64.encode(&data), "base64"],
        "owner": program.to_string(),
        "executable": false,
        "space": data.len(),
    })))
}

/// Scripts the reads a USDC transfer from the fixture wallet needs.
pub(crate) fn stock_usdc(node: &FakeNode, amount: u64, should_create: bool) -> Fallible<()> {
    stock_token(node, TokenId::UsdcSpl, amount, should_create)
}

pub(crate) fn stock_token(
    node: &FakeNode,
    token: TokenId,
    amount: u64,
    should_create: bool,
) -> Fallible<()> {
    let source = token_account_address(OWNER, token)?;
    let destination = token_account_address(PEER, token)?;
    node.at(
        Method::GetAccountInfo,
        &source,
        Answer::ok(&token_account(OWNER, token.contract(), amount)?),
    );
    node.at(
        Method::GetAccountInfo,
        &destination,
        if should_create {
            Answer::ok(&missing_account())
        } else {
            Answer::ok(&token_account(PEER, token.contract(), 0)?)
        },
    );
    if is_token_2022(token) {
        let recorded: Value = serde_json::from_str(include_str!("../../fixtures/mints.json"))?;
        let name = token.symbol().to_ascii_lowercase();
        node.at(
            Method::GetAccountInfo,
            token.contract(),
            Answer::ok(&contextual(recorded.get(&name).ok_or("the recorded mint is present")?)),
        );
    }
    // Mainnet charges 6,960 lamports a byte, counting 128 bytes of account overhead.
    let rent_lamports = account_bytes(token).saturating_add(128).saturating_mul(6_960);
    node.at(
        Method::GetMinimumBalanceForRentExemption,
        &account_bytes(token).to_string(),
        Answer::ok(&json!(rent_lamports)),
    );
    node.at(Method::GetMinimumBalanceForRentExemption, "0", Answer::ok(&json!(890_880u64)));
    Ok(())
}

/// One `getSignatureStatuses` answer, each status at a `confirmationStatus`
/// such as `"confirmed"`.
pub(crate) fn statuses(rows: &[Option<(u64, &str, bool)>]) -> Value {
    let value: Vec<Value> = rows
        .iter()
        .map(|row| match *row {
            None => Value::Null,
            Some((slot, level, is_failed)) => json!({
                "slot": slot,
                "confirmations": Value::Null,
                "confirmationStatus": level,
                "err": if is_failed { json!({"InstructionError": [0, {"Custom": 1}]}) } else { Value::Null },
            }),
        })
        .collect();
    contextual(&Value::Array(value))
}

/// One preview or quote detail row, by label.
pub(crate) fn detail(details: &[TxDetail], label: &str) -> Option<String> {
    details.iter().find(|row| row.label == label).map(|row| row.value.clone())
}

pub(crate) fn warns(warnings: &[TxWarning], kind: TxWarningKind) -> bool {
    warnings.iter().any(|warning| warning.kind == kind)
}

/// Builds and signs a request, then parses the wire bytes with the ecosystem's
/// own deserialiser rather than the encoder under test.
pub(crate) async fn signed(
    harness: &Harness,
    request: &TxRequest,
) -> Fallible<VersionedTransaction> {
    let unsigned = harness.adapter.build_tx(request).await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[owner_key()])?;
    wire(&signed)
}

pub(crate) fn wire(signed: &SignedTx) -> Fallible<VersionedTransaction> {
    Ok(wincode::deserialize_exact(signed.raw.as_bytes())?)
}

/// Every instruction of a wire transaction as (program address, data hex).
pub(crate) fn instructions(transaction: &VersionedTransaction) -> Vec<(String, String)> {
    let VersionedMessage::V0(message) = &transaction.message else { return Vec::new() };
    message
        .instructions
        .iter()
        .map(|instruction| {
            let program = message
                .account_keys
                .get(usize::from(instruction.program_id_index))
                .map_or_else(String::new, ToString::to_string);
            (program, hex::encode(&instruction.data))
        })
        .collect()
}

/// The accounts one instruction of a wire transaction names.
pub(crate) fn accounts_of(transaction: &VersionedTransaction, index: usize) -> Vec<String> {
    let VersionedMessage::V0(message) = &transaction.message else { return Vec::new() };
    let Some(instruction) = message.instructions.get(index) else { return Vec::new() };
    instruction
        .accounts
        .iter()
        .map(|at| {
            message.account_keys.get(usize::from(*at)).map_or_else(String::new, ToString::to_string)
        })
        .collect()
}

pub(crate) const fn v0_of(transaction: &VersionedTransaction) -> Option<&v0::Message> {
    let VersionedMessage::V0(message) = &transaction.message else { return None };
    Some(message)
}

/// A pinned vector value, by path, as text.
pub(crate) fn vector(path: &[&str]) -> Option<String> {
    let root: Value = serde_json::from_str(VECTORS).ok()?;
    let mut node = &root;
    for key in path {
        node = node.get(key)?;
    }
    Some(node.as_str().map_or_else(|| node.to_string(), str::to_owned))
}

/// A pinned vector number, by path.
pub(crate) fn vector_number(path: &[&str]) -> Option<u64> {
    vector(path)?.parse().ok()
}
