use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, Clock as _, CursorScope, CursorStore,
    FeeSelection, KeyError, KeyMaterial, ReservationSlot, ScriptKind, SendAmount, TokenId,
    TxRequest, WatchedAddress,
    test_util::{FakeClock, MemoryCursorStore, WatchedAddressBuilder},
};
use jelly_keypair::Ed25519Key;
use jelly_ton::{ENTRY, Method, TonAdapter, TonError, TonNode};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tonlib_core::cell::{Cell, CellBuilder};
use tonlib_core::tlb_types::tlb::TLB as _;
use tonlib_core::types::TonAddress;
use tonlib_core::wallet::mnemonic::KeyPair;
use tonlib_core::wallet::ton_wallet::TonWallet;
use tonlib_core::wallet::version_helper::VersionHelper;
use tonlib_core::wallet::versioned::v3::WalletDataV3;
use tonlib_core::wallet::versioned::v4::WalletDataV4;
use tonlib_core::wallet::versioned::v5::WalletDataV5;
use tonlib_core::wallet::wallet_version::WalletVersion;

pub(crate) const SECRET: &str = "3570e35f54cfb843f2cfaf2b8cae7ceeb7b32225d7dbbd86f611056d74d9073e";
pub(crate) const RECIPIENT: &str = "EQBe6DtCpJZe8M4t-crMXe93JlEYgSl30S5OUuMSLOfeQfBu";
pub(crate) const HISTORY_PUBKEY: [u8; 32] = [
    0x51, 0xea, 0x5f, 0xaa, 0x86, 0x26, 0x52, 0x17, 0x2a, 0x96, 0x92, 0x54, 0xf0, 0x3f, 0x45, 0x7e,
    0x30, 0x5a, 0x12, 0x05, 0x64, 0xb2, 0xb6, 0xfb, 0xb7, 0xb4, 0x21, 0x5f, 0xb3, 0x5e, 0x20, 0x02,
];
pub(crate) const HISTORY_OWNER: &str = "UQDv2YSmlrlLH3hLNOVxC8FcQf4F9eGNs4vb2zKma4txo6i3";
/// The wallet-core test key whose V5R1 wallet holds USDT on mainnet.
pub(crate) const USDT_SECRET: &str =
    "502d60b0f3327382e7d0585b789f1db9aa04907fe5cddc5c28818ec163ebf4ba";
pub(crate) const USDT_OWNER: &str = "UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_";
pub(crate) const USDT_WALLET: &str = "EQDg4AjfaxQBVsUFueenkKlHLhhYWrcBvCEzbEgfrT0nxuGC";
/// 32 zero bytes in base64, the hash of a position that names no transaction.
pub(crate) const ZERO_HASH: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

/// One jetton wallet the fake master names for the watched owner.
#[derive(Debug)]
pub(crate) struct FakeJetton {
    pub(crate) token: TokenId,
    pub(crate) wallet: TonAddress,
    pub(crate) status: String,
    pub(crate) balance: u128,
    /// The owner the wallet's `get_wallet_data` names, or the watched wallet for `None`.
    pub(crate) owner: Option<TonAddress>,
    pub(crate) position: Value,
    pub(crate) pages: VecDeque<Value>,
}

#[derive(Debug)]
pub(crate) struct State {
    /// The watched wallet, the default key's V5R1 wallet until an adapter starts watching
    /// another. Its version decides the code and data it reports.
    pub(crate) sender: TonAddress,
    pub(crate) version: ScriptKind,
    pub(crate) balance_nanogram: u64,
    pub(crate) seqno: u32,
    pub(crate) account_status: String,
    pub(crate) recipient_status: String,
    pub(crate) is_signature_allowed: bool,
    pub(crate) has_submission_error: bool,
    pub(crate) is_stale: bool,
    pub(crate) fees: Value,
    pub(crate) position: Value,
    pub(crate) pages: VecDeque<Value>,
    pub(crate) jettons: Vec<FakeJetton>,
    /// Addresses the probe finds in use.
    pub(crate) used: Vec<TonAddress>,
    pub(crate) calls: Vec<(Method, Value)>,
    pub(crate) submissions: Vec<String>,
}

impl Default for State {
    fn default() -> Self {
        let jettons = [(TokenId::EnaJetton, 3), (TokenId::UsdtJetton, 1), (TokenId::XautJetton, 2)]
            .map(|(token, seed)| FakeJetton {
                token,
                wallet: TonAddress::new(0, [seed; 32].into()),
                status: String::from("uninitialized"),
                balance: 0,
                owner: None,
                position: json!({"lt":"0","hash":ZERO_HASH}),
                pages: VecDeque::new(),
            })
            .into();
        Self {
            sender: watched()
                .ok()
                .and_then(|watched| TonAddress::from_base64_url(watched.address.value()).ok())
                .unwrap_or(TonAddress::NULL),
            version: ScriptKind::TonV5r1,
            balance_nanogram: 5_000_000_000,
            seqno: 1,
            account_status: String::from("active"),
            recipient_status: String::from("active"),
            is_signature_allowed: true,
            has_submission_error: false,
            is_stale: false,
            fees: json!({"in_fwd_fee":100_000,"storage_fee":"10","gas_fee":200_000,"fwd_fee":100_000}),
            position: json!({"lt":"0","hash":ZERO_HASH}),
            pages: VecDeque::new(),
            jettons,
            used: Vec::new(),
            calls: Vec::new(),
            submissions: Vec::new(),
        }
    }
}

impl State {
    pub(crate) fn jetton(&mut self, token: TokenId) -> Result<&mut FakeJetton, TonError> {
        self.jettons.iter_mut().find(|jetton| jetton.token == token).ok_or(TonError::Protocol)
    }

    fn sender_account(&self, sync_utime: i64) -> Result<Value, TonError> {
        let pubkey = Ed25519Key::from_key_material(&key_of(SECRET)?)?.public_bytes();
        let version = match self.version {
            ScriptKind::TonV4r2 => WalletVersion::V4R2,
            ScriptKind::TonV3r2 => WalletVersion::V3R2,
            ScriptKind::TonV5r1
            | ScriptKind::P2tr
            | ScriptKind::P2wpkh
            | ScriptKind::P2shP2wpkh
            | ScriptKind::P2pkh => WalletVersion::V5R1,
        };
        let wallet = TonWallet::new(
            version,
            KeyPair { public_key: pubkey.to_vec(), secret_key: Vec::new() },
        )?;
        let data = match self.version {
            ScriptKind::TonV4r2 => {
                let mut data = WalletDataV4::new(wallet.wallet_id, pubkey.into());
                data.seqno = self.seqno;
                data.to_boc(true)?
            }
            ScriptKind::TonV3r2 => {
                let mut data = WalletDataV3::new(wallet.wallet_id, pubkey.into());
                data.seqno = self.seqno;
                data.to_boc(true)?
            }
            ScriptKind::TonV5r1
            | ScriptKind::P2tr
            | ScriptKind::P2wpkh
            | ScriptKind::P2shP2wpkh
            | ScriptKind::P2pkh => {
                let mut data = WalletDataV5::new(wallet.wallet_id, pubkey.into());
                data.seqno = self.seqno;
                data.signature_allowed = self.is_signature_allowed;
                data.to_boc(true)?
            }
        };
        Ok(json!({"balance":self.balance_nanogram.to_string(), "state":self.account_status,
            "code":STANDARD.encode(VersionHelper::get_code(wallet.version)?.to_boc(true)?),
            "data":STANDARD.encode(data),"block_id":{"seqno":100},
            "sync_utime":sync_utime,"last_transaction_id":self.position}))
    }
}

fn address_param(params: &Value, field: &str) -> Result<TonAddress, TonError> {
    let text = params.get(field).and_then(Value::as_str).ok_or(TonError::Protocol)?;
    TonAddress::from_base64_url(text)
        .or_else(|_| TonAddress::from_hex_str(text))
        .map_err(|_| TonError::Protocol)
}

fn cell(address: &TonAddress) -> Result<Value, TonError> {
    let mut builder = CellBuilder::new();
    builder.store_address(address)?;
    Ok(json!(["cell", {"bytes": STANDARD.encode(builder.build()?.to_boc(false)?)}]))
}

/// The owner a `get_wallet_address` call asks about, out of its one-slice stack.
fn stack_owner(params: &Value) -> Result<TonAddress, TonError> {
    let encoded = params.pointer("/stack/0/1").and_then(Value::as_str).ok_or(TonError::Protocol)?;
    let bytes = STANDARD.decode(encoded).map_err(|_| TonError::Protocol)?;
    Ok(Cell::from_boc(&bytes)?.parser().load_address()?)
}

/// The jetton wallet the fake master names for an owner other than the watched wallet.
pub(crate) fn jetton_wallet_of(token: TokenId, owner: &TonAddress) -> TonAddress {
    let mut hash = owner.hash_part.clone();
    if let Some(first) = hash.as_mut_slice().first_mut() {
        *first ^= if token == TokenId::UsdtJetton { 0x5a } else { 0xa5 };
    }
    TonAddress::new(0, hash)
}

fn run_result(stack: &Value) -> Value {
    json!({"stack": stack, "exit_code": 0, "block_id": {"seqno": 100}})
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
            clock: Arc::new(FakeClock::new(1_723_469_363_000)),
            cursors: Arc::new(MemoryCursorStore::new()),
        })
    }

    /// An adapter over this node and its clock that keeps its state in `cursors` and
    /// watches nothing yet.
    pub(crate) fn adapter_over(self: &Arc<Self>, cursors: Arc<dyn CursorStore>) -> TonAdapter {
        TonAdapter::new(Arc::<Self>::clone(self), cursors, Arc::<FakeClock>::clone(&self.clock))
    }

    /// An adapter over this node, its clock, and its store that watches the default key's
    /// V5R1 wallet.
    pub(crate) async fn adapter(self: &Arc<Self>) -> Result<TonAdapter, ChainError> {
        self.adapter_watching(watched().map_err(TonError::from)?).await
    }

    /// An adapter over this node, its clock, and its store that watches `watched`, which
    /// the node then answers for as the sender.
    pub(crate) async fn adapter_watching(
        self: &Arc<Self>,
        watched: WatchedAddress,
    ) -> Result<TonAdapter, ChainError> {
        self.state.lock().sender =
            TonAddress::from_base64_url(watched.address.value()).map_err(|_| TonError::Protocol)?;
        let adapter = self.adapter_over(Arc::<MemoryCursorStore>::clone(&self.cursors));
        adapter.set_watch_set(&[watched]).await?;
        Ok(adapter)
    }

    pub(crate) fn count(&self, method: Method) -> usize {
        self.state.lock().calls.iter().filter(|(called, _)| *called == method).count()
    }
}

#[async_trait]
impl TonNode for Node {
    async fn call(&self, method: Method, params: Value) -> Result<Value, TonError> {
        let mut state = self.state.lock();
        state.calls.push((method, params.clone()));
        match method {
            Method::GetMasterchainInfo => Ok(json!({"last":{"seqno":100},"init":{
                "root_hash":"F6OpKZKqvqeFp6CQmFomXNMfMj2EnaUSOXN+Mh+wVWk=",
                "file_hash":"XplPz01CXAps5qeSWUtxcyBfdAo5zVb1N979KLSKD24="}})),
            Method::GetAddressInformation => {
                let address = address_param(&params, "address")?;
                let sync_utime =
                    self.clock.now_secs()?.saturating_sub(if state.is_stale { 1000 } else { 0 });
                if address == state.sender {
                    return state.sender_account(sync_utime);
                }
                if let Some(jetton) = state.jettons.iter().find(|jetton| jetton.wallet == address) {
                    return Ok(
                        json!({"balance":"50000000","state":jetton.status,"code":"","data":"",
                        "block_id":{"seqno":100},"sync_utime":sync_utime,
                        "last_transaction_id":jetton.position}),
                    );
                }
                let recipient =
                    TonAddress::from_base64_url(RECIPIENT).map_err(|_| TonError::Protocol)?;
                let (status, balance, lt) = if state.used.contains(&address) {
                    ("active", "1", "5")
                } else if address == recipient {
                    (state.recipient_status.as_str(), "0", "0")
                } else {
                    ("uninitialized", "0", "0")
                };
                Ok(json!({"balance":balance,"state":status,"code":"","data":"",
                    "block_id":{"seqno":100},"sync_utime":sync_utime,
                    "last_transaction_id":{"lt":lt,"hash":ZERO_HASH}}))
            }
            Method::RunGet => {
                let address = address_param(&params, "address")?;
                match params.get("method").and_then(Value::as_str) {
                    Some("get_wallet_address") => {
                        let jetton = state
                            .jettons
                            .iter()
                            .find(|jetton| {
                                TonAddress::from_base64_url(jetton.token.contract()).ok()
                                    == Some(address.clone())
                            })
                            .ok_or(TonError::Protocol)?;
                        let owner = stack_owner(&params)?;
                        let wallet = if owner == state.sender {
                            jetton.wallet.clone()
                        } else {
                            jetton_wallet_of(jetton.token, &owner)
                        };
                        Ok(run_result(&json!([cell(&wallet)?])))
                    }
                    Some("get_wallet_data") => {
                        let jetton = state
                            .jettons
                            .iter()
                            .find(|jetton| jetton.wallet == address)
                            .ok_or(TonError::Protocol)?;
                        let master = TonAddress::from_base64_url(jetton.token.contract())
                            .map_err(|_| TonError::Protocol)?;
                        let owner = jetton.owner.clone().unwrap_or_else(|| state.sender.clone());
                        Ok(run_result(&json!([
                            ["num", format!("0x{:x}", jetton.balance)],
                            cell(&owner)?,
                            cell(&master)?,
                            cell(&jetton.wallet)?
                        ])))
                    }
                    _ => Err(TonError::Protocol),
                }
            }
            Method::EstimateFee => Ok(json!({"source_fees":state.fees})),
            Method::GetTransactions => {
                let address = address_param(&params, "address")?;
                if address == state.sender {
                    return Ok(state.pages.pop_front().unwrap_or_else(|| json!([])));
                }
                let jetton = state
                    .jettons
                    .iter_mut()
                    .find(|jetton| jetton.wallet == address)
                    .ok_or(TonError::Protocol)?;
                Ok(jetton.pages.pop_front().unwrap_or_else(|| json!([])))
            }
            Method::SendBoc => Err(TonError::Protocol),
        }
    }

    async fn broadcast(&self, boc: &str) -> Result<(), TonError> {
        let saved = self
            .cursors
            .load_reservation(CursorScope::Address(1), ReservationSlot::TonSends)
            .await?
            .ok_or(TonError::Protocol)?;
        if !saved.with_bytes(|bytes| String::from_utf8_lossy(bytes).contains(boc)) {
            return Err(TonError::Protocol);
        }
        let mut state = self.state.lock();
        state.submissions.push(boc.to_owned());
        if state.has_submission_error {
            return Err(TonError::Protocol);
        }
        Ok(())
    }
}

pub(crate) fn key_of(secret: &str) -> Result<KeyMaterial, KeyError> {
    Ok(ENTRY.import_key(secret, None)?.material)
}

pub(crate) fn key() -> Result<KeyMaterial, KeyError> {
    key_of(SECRET)
}

pub(crate) fn watched_as(secret: &str, version: ScriptKind) -> Result<WatchedAddress, KeyError> {
    let key = key_of(secret)?;
    let mut watched =
        WatchedAddressBuilder::new(1, ChainId::Ton, &ENTRY.address(&key, Some(version))?).build();
    watched.pubkey = Some(Ed25519Key::from_key_material(&key)?.public_bytes());
    Ok(watched)
}

pub(crate) fn watched() -> Result<WatchedAddress, KeyError> {
    watched_as(SECRET, ScriptKind::TonV5r1)
}

pub(crate) fn history_watched() -> WatchedAddress {
    let mut watched = WatchedAddressBuilder::new(1, ChainId::Ton, HISTORY_OWNER).build();
    watched.pubkey = Some(HISTORY_PUBKEY);
    watched
}

#[track_caller]
pub(crate) fn assert_refused(error: &ChainError, message: &str) {
    assert!(
        matches!(error, ChainError::InvalidInput(text, _) if text == message),
        "expected {message:?}, got {error:?}"
    );
}

pub(crate) fn request() -> TxRequest {
    TxRequest {
        from_address_ids: vec![1],
        to: RECIPIENT.to_owned(),
        destination_tag: None,
        comment: None,
        should_delete_account: false,
        asset: AssetId::Native(ChainId::Ton),
        amount: SendAmount::Exact { value: Amount::from_base_units(10) },
        fee: FeeSelection::NORMAL,
        nonce: None,
        data: None,
    }
}

pub(crate) fn jetton_request(token: TokenId, units: u128) -> TxRequest {
    TxRequest {
        asset: AssetId::Token(token),
        amount: SendAmount::Exact { value: Amount::from_base_units(units) },
        ..request()
    }
}
