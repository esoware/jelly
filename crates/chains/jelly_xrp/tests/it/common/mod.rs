use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, Clock as _, CursorScope, CursorStore,
    FeeSelection, KeyKind, KeyMaterial, ReservationSlot, SendAmount, TxRequest, WatchedAddress,
    test_util::{FakeClock, MemoryCursorStore, WatchedAddressBuilder, key_material_of},
};
use jelly_xrp::{Method, XrpAdapter, XrpError, XrpNode};
use parking_lot::Mutex;
use serde_json::{Value, json};

pub(crate) const OWNER: &str = "rfxdLwsZnoespnTDDb1Xhvbc8EFNdztaoq";
pub(crate) const RECIPIENT: &str = "rU893viamSnsfP3zjzM2KPxjqZjXSXK6VF";
const SECRET: &str = "a5576c0f63da10e584568c8d134569ff44017b0a249eb70657127ae04f38cc77";
pub(crate) const BUSY: &str = "Wait for the pending XRP payment to finish before sending again.";
const RIPPLE_EPOCH_UNIX_SECONDS: i64 = 946_684_800;
// AccountRoot flags, as the XRP Ledger defines them.
pub(crate) const LSF_REQUIRE_DEST_TAG: u32 = 0x0002_0000;
pub(crate) const LSF_DISABLE_MASTER: u32 = 0x0010_0000;
pub(crate) const LSF_DEPOSIT_AUTH: u32 = 0x0100_0000;

#[derive(Debug)]
pub(crate) struct Recipient {
    pub(crate) flags: u32,
    pub(crate) is_funded: bool,
    pub(crate) is_authorized: bool,
}

#[derive(Debug)]
pub(crate) struct Sender {
    pub(crate) flags: u32,
    pub(crate) sequence: u32,
    /// Whether the account exists, which it stops doing once deleted.
    pub(crate) is_funded: bool,
}

#[derive(Debug)]
pub(crate) struct State {
    pub(crate) balance_drops: u64,
    pub(crate) reserve_base: u64,
    pub(crate) reserve_inc: u64,
    pub(crate) owner_count: u32,
    pub(crate) sender: Sender,
    /// The `account_objects` pages a deletion check reads, in order. Once they run out the
    /// account owns no blocker.
    pub(crate) object_pages: VecDeque<Value>,
    pub(crate) recipient: Recipient,
    pub(crate) is_validated: bool,
    pub(crate) fee: String,
    pub(crate) ledger: u32,
    pub(crate) has_submission_error: bool,
    pub(crate) engine_result: String,
    pub(crate) has_searched_all: bool,
    pub(crate) tx: Option<Value>,
    pub(crate) pages: VecDeque<Value>,
    pub(crate) calls: Vec<(Method, Value)>,
    pub(crate) submissions: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            balance_drops: 10_000_000,
            reserve_base: 1_000_000,
            reserve_inc: 200_000,
            owner_count: 2,
            sender: Sender { flags: 0, sequence: 7, is_funded: true },
            object_pages: VecDeque::new(),
            recipient: Recipient { flags: 0, is_funded: true, is_authorized: false },
            is_validated: true,
            fee: String::from("10"),
            ledger: 90_000_000,
            has_submission_error: false,
            engine_result: String::from("tesSUCCESS"),
            has_searched_all: false,
            tx: None,
            pages: VecDeque::new(),
            calls: Vec::new(),
            submissions: Vec::new(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) state: Mutex<State>,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) cursors: Arc<MemoryCursorStore>,
}

impl Node {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State::default()),
            clock: Arc::new(FakeClock::default()),
            cursors: Arc::new(MemoryCursorStore::new()),
        })
    }

    /// An adapter over this node and its clock that keeps its state in `cursors` and
    /// watches nothing yet.
    pub(crate) fn adapter_over(self: &Arc<Self>, cursors: Arc<dyn CursorStore>) -> XrpAdapter {
        XrpAdapter::new(Arc::<Self>::clone(self), cursors, Arc::<FakeClock>::clone(&self.clock))
    }

    /// An adapter over this node, its clock, and its store that watches the owner.
    pub(crate) async fn adapter(self: &Arc<Self>) -> Result<XrpAdapter, ChainError> {
        let adapter = self.adapter_over(Arc::<MemoryCursorStore>::clone(&self.cursors));
        adapter.set_watch_set(&[watched()]).await?;
        Ok(adapter)
    }

    /// The params of every call to `method`, oldest first.
    pub(crate) fn calls_to(&self, method: Method) -> Vec<Value> {
        self.state
            .lock()
            .calls
            .iter()
            .filter(|(called, _)| *called == method)
            .map(|(_, params)| params.clone())
            .collect()
    }
}

#[async_trait]
impl XrpNode for Node {
    async fn call(&self, method: Method, params: Value) -> Result<Value, XrpError> {
        let mut state = self.state.lock();
        state.calls.push((method, params.clone()));
        match method {
            Method::ServerState => {
                let now_unix_secs = self.clock.now_secs()?;
                Ok(json!({"state":{"validated_ledger":{
                    "seq":state.ledger, "hash":"AB".repeat(32), "reserve_base":state.reserve_base,
                    "reserve_inc":state.reserve_inc,
                    "close_time":now_unix_secs.saturating_sub(RIPPLE_EPOCH_UNIX_SECONDS),
                }}}))
            }
            Method::AccountInfo => {
                let is_sender = params.get("account").and_then(Value::as_str) == Some(OWNER);
                if (!is_sender && !state.recipient.is_funded) || (is_sender && !state.sender.is_funded) {
                    return Ok(json!({"error":"actNotFound","status":"error"}));
                }
                let sequence = if is_sender { state.sender.sequence } else { 7 };
                Ok(
                    json!({"validated":state.is_validated,"ledger_index":state.ledger,"account_data":{
                        "Account":params.get("account"),"Balance":state.balance_drops.to_string(),"Sequence":sequence,
                        "OwnerCount":if is_sender {state.owner_count} else {0},
                        "Flags":if is_sender {state.sender.flags} else {state.recipient.flags},
                    }}),
                )
            }
            Method::Fee => Ok(json!({"drops":{"minimum_fee":"10","open_ledger_fee":state.fee}})),
            Method::DepositAuthorized => Ok(json!({"deposit_authorized":state.recipient.is_authorized})),
            Method::AccountObjects => Ok(state.object_pages.pop_front().unwrap_or_else(|| {
                json!({"account":params.get("account"),"account_objects":[],"validated":true})
            })),
            Method::Tx => Ok(state.tx.clone().unwrap_or_else(
                || json!({"error":"txnNotFound","searched_all":state.has_searched_all}),
            )),
            Method::AccountTx => Ok(state.pages.pop_front().unwrap_or_else(|| {
                json!({
                    "account":OWNER,"validated":true,"ledger_index_min":params.get("ledger_index_min"),
                    "ledger_index_max":params.get("ledger_index_max"),"transactions":[],
                })
            })),
            Method::ServerInfo | Method::Submit => Err(XrpError::Protocol),
        }
    }

    async fn broadcast(&self, blob: &str) -> Result<Value, XrpError> {
        self.state.lock().submissions.push(blob.to_owned());
        let saved = self
            .cursors
            .load_reservation(CursorScope::Address(1), ReservationSlot::XrpSends)
            .await?
            .ok_or(XrpError::Protocol)?;
        if !saved.with_bytes(|bytes| String::from_utf8_lossy(bytes).contains(blob)) {
            return Err(XrpError::Protocol);
        }
        let state = self.state.lock();
        if state.has_submission_error {
            return Err(XrpError::Protocol);
        }
        Ok(json!({"engine_result":state.engine_result}))
    }
}

#[track_caller]
pub(crate) fn assert_refused(error: &ChainError, message: &str) {
    assert!(
        matches!(error, ChainError::InvalidInput(text, _) if text == message),
        "expected {message:?}, got {error:?}"
    );
}

pub(crate) fn key() -> Result<KeyMaterial, hex::FromHexError> {
    Ok(key_material_of(KeyKind::Secp256k1, hex::decode(SECRET)?))
}

pub(crate) fn watched() -> WatchedAddress {
    WatchedAddressBuilder::new(1, ChainId::Xrp, OWNER).build()
}

pub(crate) fn request() -> TxRequest {
    TxRequest {
        from_address_ids: vec![1],
        to: RECIPIENT.to_owned(),
        destination_tag: None,
        comment: None,
        should_delete_account: false,
        asset: AssetId::Native(ChainId::Xrp),
        amount: SendAmount::Exact { value: Amount::from_base_units(2_000_000) },
        fee: FeeSelection::NORMAL,
        nonce: None,
        data: None,
    }
}

pub(crate) fn deletion() -> TxRequest {
    TxRequest { should_delete_account: true, amount: SendAmount::Max, ..request() }
}

pub(crate) fn transaction(hash: &str, result: &str, before_drops: u64, after_drops: u64) -> Value {
    json!({"validated":true,"ledger_index":90_000_001,"hash":hash,"Account":OWNER,"Destination":RECIPIENT,
    "Amount":"2000000","Fee":"10","date":800_000_000,"meta":{
        "TransactionResult":result,"AffectedNodes":[{"ModifiedNode":{
            "LedgerEntryType":"AccountRoot","FinalFields":{"Account":OWNER,"Balance":after_drops.to_string()},
            "PreviousFields":{"Balance":before_drops.to_string()},
        }}],
    }})
}
