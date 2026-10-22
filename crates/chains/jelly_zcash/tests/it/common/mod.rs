use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, Clock as _, CursorScope, CursorStore,
    FeeSelection, KeyMaterial, ReservationSlot, SendAmount, TxRequest, WatchedAddress,
    test_util::{FakeClock, MemoryCursorStore, WatchedAddressBuilder},
};
use jelly_keypair::Secp256k1Key;
use jelly_zcash::{ENTRY, Route, ZcashAdapter, ZcashError, ZcashNode};
use parking_lot::Mutex;
use serde_json::{Value, json};
use zcash_primitives::transaction::{Authorized, Transaction, TransactionData, TxVersion};
use zcash_protocol::{
    consensus::{BlockHeight, BranchId, MAIN_NETWORK},
    value::Zatoshis,
};
use zcash_script::script::Code;
use zcash_transparent::{
    address::Script,
    bundle::{self, Bundle, OutPoint, TxIn, TxOut},
};

pub(crate) const CHAIN: ChainId = ChainId::Zcash;
pub(crate) const OWNER: &str = "t1Wg9uPPAfwhBWeRjtDPa5ZHNzyBx9rJVKY";
pub(crate) const RECIPIENT: &str = "t1VmmGiyjVNeCjxDZzg7vZmd99WyzVby9yC";
pub(crate) const HEIGHT: u32 = 3_514_158;

pub(crate) fn key() -> Result<KeyMaterial, ZcashError> {
    Ok(ENTRY
        .import_key("2d8f68944bdbfbc0769542fba8fc2d2a3de67393334471624364c7006da2aa54", None)?
        .material)
}

/// A v5 transaction spending `inputs` into one output of `value_zat` locked by `script`.
fn spending(
    inputs: Vec<OutPoint>,
    script: Vec<u8>,
    value_zat: u64,
) -> Result<Transaction, ZcashError> {
    TransactionData::<Authorized>::from_parts(
        TxVersion::V5,
        BranchId::for_height(&MAIN_NETWORK, BlockHeight::from_u32(HEIGHT)),
        0,
        BlockHeight::from_u32(HEIGHT.saturating_add(40)),
        Some(Bundle {
            vin: inputs
                .into_iter()
                .map(|input| TxIn::from_parts(input, Script(Code(vec![0, 0])), u32::MAX))
                .collect(),
            vout: vec![TxOut::new(
                Zatoshis::from_u64(value_zat).map_err(|_| ZcashError::Protocol)?,
                Script(Code(script)),
            )],
            authorization: bundle::Authorized,
        }),
        None,
        None,
        None,
    )
    .freeze()
    .map_err(|_| ZcashError::Protocol)
}

pub(crate) fn transaction(
    input: OutPoint,
    address: &str,
    value_zat: u64,
) -> Result<Transaction, ZcashError> {
    consolidation(vec![input], address, value_zat)
}

pub(crate) fn consolidation(
    inputs: Vec<OutPoint>,
    address: &str,
    value_zat: u64,
) -> Result<Transaction, ZcashError> {
    let script = ENTRY.codec().script_pubkey(address).map_err(|_| ZcashError::Protocol)?;
    spending(inputs, script, value_zat)
}

/// Pays `value_zat` to the owner's compressed public key with a bare pay-to-pubkey script,
/// which Blockbook indexes under the owner's key-hash address.
pub(crate) fn pay_to_owner_pubkey(
    input: OutPoint,
    value_zat: u64,
) -> Result<Transaction, ZcashError> {
    let pubkey = Secp256k1Key::from_key_material(&key()?)?.public_compressed();
    let mut script = vec![33];
    script.extend_from_slice(&pubkey);
    script.push(0xac);
    spending(vec![input], script, value_zat)
}

pub(crate) fn document(tx: &Transaction, height: Option<u32>) -> Result<Value, ZcashError> {
    let mut raw = Vec::new();
    tx.write(&mut raw).map_err(|_| ZcashError::Protocol)?;
    Ok(json!({"txid":tx.txid().to_string(),"hex":hex::encode(raw),"blockHeight":height,
        "confirmations":u32::from(height.is_some()),"blockTime":1_791_729_061}))
}

#[derive(Debug)]
pub(crate) struct State {
    pub(crate) height: u32,
    pub(crate) backend_lead_blocks: u32,
    pub(crate) hash: String,
    pub(crate) branch: Option<String>,
    pub(crate) is_synced: bool,
    pub(crate) block_age_secs: i64,
    pub(crate) coins: Value,
    /// The unspent outputs of addresses other than the owner's, by address.
    pub(crate) address_coins: BTreeMap<String, Value>,
    pub(crate) transactions: BTreeMap<String, Value>,
    pub(crate) history: Vec<String>,
    pub(crate) balance: String,
    pub(crate) unconfirmed_balance: String,
    pub(crate) has_submission_error: bool,
    pub(crate) has_wrong_txid: bool,
    pub(crate) page_size: usize,
    pub(crate) missing_history_count: usize,
    pub(crate) submissions: Vec<String>,
    pub(crate) history_pages: Vec<u32>,
}

#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) state: Mutex<State>,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) cursors: Arc<MemoryCursorStore>,
    pub(crate) funding: Transaction,
}

impl Node {
    pub(crate) fn new() -> Result<Arc<Self>, ZcashError> {
        let parent = transaction(OutPoint::NULL, RECIPIENT, 1_010_000)?;
        let funding = transaction(OutPoint::new(*parent.txid().as_ref(), 0), OWNER, 1_000_000)?;
        let txid = funding.txid().to_string();
        Ok(Arc::new(Self {
            state: Mutex::new(State {
                height: HEIGHT,
                backend_lead_blocks: 0,
                hash: "ab".repeat(32),
                branch: None,
                is_synced: true,
                block_age_secs: 0,
                coins: json!([{"txid":txid,"vout":0,"value":"1000000"}]),
                address_coins: BTreeMap::new(),
                transactions: BTreeMap::from([
                    (
                        parent.txid().to_string(),
                        document(&parent, Some(HEIGHT.saturating_sub(10)))?,
                    ),
                    (txid.clone(), document(&funding, Some(HEIGHT.saturating_sub(5)))?),
                ]),
                history: vec![txid],
                balance: String::from("1000000"),
                unconfirmed_balance: String::from("0"),
                has_submission_error: false,
                has_wrong_txid: false,
                page_size: 1_000,
                missing_history_count: 0,
                submissions: Vec::new(),
                history_pages: Vec::new(),
            }),
            clock: Arc::new(FakeClock::default()),
            cursors: Arc::new(MemoryCursorStore::new()),
            funding,
        }))
    }

    /// An adapter over this node and its clock that keeps its state in `cursors` and
    /// watches nothing yet.
    pub(crate) fn adapter_over(self: &Arc<Self>, cursors: Arc<dyn CursorStore>) -> ZcashAdapter {
        ZcashAdapter::new(Arc::<Self>::clone(self), cursors, Arc::<FakeClock>::clone(&self.clock))
    }

    /// An adapter over this node, its clock, and its store that watches the owner.
    pub(crate) async fn adapter(self: &Arc<Self>) -> Result<ZcashAdapter, ChainError> {
        let adapter = self.adapter_over(Arc::<MemoryCursorStore>::clone(&self.cursors));
        adapter.set_watch_set(&[watched()]).await?;
        Ok(adapter)
    }
}

#[async_trait]
impl ZcashNode for Node {
    async fn get(&self, route: Route<'_>) -> Result<Value, ZcashError> {
        let mut state = self.state.lock();
        match route {
            Route::Status => {
                let branch = state.branch.clone().unwrap_or_else(|| {
                    format!(
                        "{:08x}",
                        u32::from(BranchId::for_height(
                            &MAIN_NETWORK,
                            BlockHeight::from_u32(state.height.saturating_add(1))
                        ))
                    )
                });
                Ok(
                    json!({"blockbook":{"coin":"Zcash","bestHeight":state.height,"inSync":state.is_synced,"initialSync":false,"inSyncMempool":true},
                    "backend":{"chain":"main","blocks":state.height.saturating_add(state.backend_lead_blocks),"bestBlockHash":state.hash,"consensus":{"nextblock":branch}}}),
                )
            }
            Route::BlockHash(_) => Ok(json!({"blockHash":state.hash})),
            Route::Block(_) => Ok(
                json!({"hash":state.hash,"time":self.clock.now_secs().map_err(|_| ZcashError::Protocol)?.saturating_sub(state.block_age_secs)}),
            ),
            Route::Unspent(address) => {
                Ok(state.address_coins.get(address).cloned().unwrap_or_else(|| state.coins.clone()))
            }
            Route::Tx(txid) => state.transactions.get(txid).cloned().ok_or(ZcashError::Protocol),
            Route::History { page, .. } => {
                state.history_pages.push(page);
                let start = usize::try_from(page.saturating_sub(1))
                    .map_err(|_| ZcashError::Protocol)?
                    .saturating_mul(state.page_size);
                let ids: Vec<&String> =
                    state.history.iter().skip(start).take(state.page_size).collect();
                Ok(
                    json!({"page":page,"totalPages":state.history.len().div_ceil(state.page_size).max(1),"address":OWNER,
                    "balance":state.balance,"unconfirmedBalance":state.unconfirmed_balance,"txs":state.history.len().saturating_add(state.missing_history_count),
                    "unconfirmedTxs":0,"txids":ids}),
                )
            }
        }
    }

    async fn broadcast(&self, raw_hex: &str) -> Result<String, ZcashError> {
        let saved = self
            .cursors
            .load_reservation(CursorScope::Address(1), ReservationSlot::ZcashSends)
            .await?
            .ok_or(ZcashError::Protocol)?;
        if !saved.with_bytes(|bytes| String::from_utf8_lossy(bytes).contains(raw_hex)) {
            return Err(ZcashError::Protocol);
        }
        let raw = hex::decode(raw_hex).map_err(|_| ZcashError::Protocol)?;
        let tx = Transaction::read(raw.as_slice(), BranchId::Sprout)
            .map_err(|_| ZcashError::Protocol)?;
        let mut state = self.state.lock();
        state.submissions.push(raw_hex.to_owned());
        if state.has_submission_error {
            return Err(ZcashError::Protocol);
        }
        if state.has_wrong_txid { Ok("00".repeat(32)) } else { Ok(tx.txid().to_string()) }
    }
}

#[track_caller]
pub(crate) fn assert_refused(error: &ChainError, message: &str) {
    assert!(
        matches!(error, ChainError::InvalidInput(text, _) if text == message),
        "expected {message:?}, got {error:?}"
    );
}

pub(crate) fn watched() -> WatchedAddress {
    WatchedAddressBuilder::new(1, CHAIN, OWNER).build()
}

pub(crate) fn request() -> TxRequest {
    TxRequest {
        from_address_ids: vec![1],
        to: RECIPIENT.to_owned(),
        destination_tag: None,
        comment: None,
        should_delete_account: false,
        asset: AssetId::Native(CHAIN),
        amount: SendAmount::Exact { value: Amount::from_base_units(500_000) },
        fee: FeeSelection::NORMAL,
        nonce: None,
        data: None,
    }
}
