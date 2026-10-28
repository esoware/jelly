use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use jelly_core::Clock;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use zcash_primitives::transaction::{Transaction, TxVersion};
use zcash_protocol::{consensus::BranchId, value::MAX_MONEY};

use crate::address::{is_pay_to_pubkey, script};
use crate::config::{branch_at, is_mainnet_status};
use crate::error::ZcashError;
use crate::node::ZcashNode;
use crate::payload::{Coin, hash_bytes};
use crate::route::Route;
use crate::sign;
use crate::zat::add;

// Blockbook indexes a block shortly after its node has it, and a wider gap means the
// indexer is still catching up.
const MAX_INDEXER_LAG_BLOCKS: u32 = 10;
// Zcash targets a block every 75 seconds, so a tip over an hour old most likely means a
// stale endpoint. The lead allows for a local clock that runs behind the network.
const MAX_NODE_LAG_SECONDS: i64 = 3_600;
const MAX_NODE_LEAD_SECONDS: i64 = 120;
const MAX_HISTORY_PAGES: u32 = 10_000;
const MAX_UNSPENT_ROWS: usize = 100_000;

/// The transactions one read has fetched, by txid.
pub(crate) type TxCache = BTreeMap<String, Arc<TxInfo>>;

/// Every Blockbook read and the broadcast, with the checks each answer has to pass.
#[derive(Debug)]
pub(crate) struct ZcashClient {
    node: Arc<dyn ZcashNode>,
}

#[derive(Debug)]
pub(crate) struct Tip {
    pub(crate) height: u32,
    pub(crate) hash: String,
    pub(crate) branch: BranchId,
}

#[derive(Debug)]
pub(crate) struct TxInfo {
    pub(crate) tx: Transaction,
    pub(crate) height: Option<u32>,
    pub(crate) timestamp: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AddressPage {
    page: u32,
    total_pages: u32,
    address: String,
    balance: String,
    unconfirmed_balance: String,
    txs: u64,
    unconfirmed_txs: u64,
    #[serde(default)]
    pub(crate) txids: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Unspent {
    txid: String,
    vout: u32,
    value: String,
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, ZcashError> {
    serde_json::from_value(value).map_err(|_| ZcashError::Protocol)
}

fn decimal_zat(text: &str) -> Result<u64, ZcashError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(ZcashError::Protocol);
    }
    text.parse::<u64>().ok().filter(|value| *value <= MAX_MONEY).ok_or(ZcashError::Protocol)
}

impl AddressPage {
    pub(crate) fn balance_zat(&self) -> Result<u64, ZcashError> {
        let confirmed_zat = decimal_zat(&self.balance)?;
        if let Some(negative) = self.unconfirmed_balance.strip_prefix('-') {
            confirmed_zat.checked_sub(decimal_zat(negative)?).ok_or(ZcashError::Protocol)
        } else {
            add(confirmed_zat, decimal_zat(&self.unconfirmed_balance)?)
        }
    }

    pub(crate) fn is_same_snapshot(&self, other: &Self) -> bool {
        self.balance == other.balance
            && self.unconfirmed_balance == other.unconfirmed_balance
            && self.txs == other.txs
            && self.unconfirmed_txs == other.unconfirmed_txs
    }
}

impl ZcashClient {
    pub(crate) fn new(node: Arc<dyn ZcashNode>) -> Self {
        Self { node }
    }

    pub(crate) async fn tip(&self) -> Result<Tip, ZcashError> {
        let status = self.node.get(Route::Status).await?;
        if !is_mainnet_status(&status)
            || status.pointer("/blockbook/inSync").and_then(Value::as_bool) != Some(true)
            || status.pointer("/blockbook/initialSync").and_then(Value::as_bool) != Some(false)
            || status.pointer("/blockbook/inSyncMempool").and_then(Value::as_bool) != Some(true)
        {
            return Err(ZcashError::Invalid(
                "The Zcash endpoint is not synchronized. Try another endpoint.",
            ));
        }
        let height = status
            .pointer("/blockbook/bestHeight")
            .and_then(Value::as_u64)
            .and_then(|height| u32::try_from(height).ok())
            .ok_or(ZcashError::Protocol)?;
        let backend_height = status
            .pointer("/backend/blocks")
            .and_then(Value::as_u64)
            .and_then(|height| u32::try_from(height).ok())
            .ok_or(ZcashError::Protocol)?;
        if backend_height.checked_sub(height).is_none_or(|lag| lag > MAX_INDEXER_LAG_BLOCKS) {
            return Err(ZcashError::Invalid(
                "The Zcash indexer is catching up. Try again shortly.",
            ));
        }
        let hash = self.block_hash(height).await?;
        let next_height = height.checked_add(1).ok_or(ZcashError::Protocol)?;
        let branch = branch_at(next_height);
        let backend_next_height = backend_height.checked_add(1).ok_or(ZcashError::Protocol)?;
        let backend_branch = branch_at(backend_next_height);
        let expected = format!("{:08x}", u32::from(backend_branch));
        if status.pointer("/backend/consensus/nextblock").and_then(Value::as_str)
            != Some(expected.as_str())
            || !TxVersion::V5.valid_in_branch(branch)
        {
            return Err(ZcashError::Invalid(
                "This Zcash network upgrade is not supported. Update Jelly.",
            ));
        }
        if backend_branch != branch {
            return Err(ZcashError::Invalid(
                "The Zcash indexer is catching up with a network upgrade. Try again shortly.",
            ));
        }
        Ok(Tip { height, hash, branch })
    }

    /// The tip, refused unless the endpoint's block at that height has the tip's hash and
    /// a recent timestamp by `clock`.
    pub(crate) async fn fresh_tip(&self, clock: &dyn Clock) -> Result<Tip, ZcashError> {
        let tip = self.tip().await?;
        let block = self.node.get(Route::Block(tip.height)).await?;
        let block_unix_secs =
            block.get("time").and_then(Value::as_i64).ok_or(ZcashError::Protocol)?;
        let now_unix_secs = clock.now_secs()?;
        if block.get("hash").and_then(Value::as_str) != Some(tip.hash.as_str())
            || now_unix_secs.saturating_sub(block_unix_secs) > MAX_NODE_LAG_SECONDS
            || block_unix_secs.saturating_sub(now_unix_secs) > MAX_NODE_LEAD_SECONDS
        {
            return Err(ZcashError::Invalid(
                "The Zcash endpoint is not up to date. Try again shortly.",
            ));
        }
        Ok(tip)
    }

    pub(crate) async fn block_hash(&self, height: u32) -> Result<String, ZcashError> {
        let result = self.node.get(Route::BlockHash(height)).await?;
        let hash = result.get("blockHash").and_then(Value::as_str).ok_or(ZcashError::Protocol)?;
        hash_bytes(hash)?;
        Ok(hash.to_owned())
    }

    /// The transaction `txid`, from `cache` when this read has fetched it already.
    pub(crate) async fn tx(
        &self,
        txid: &str,
        cache: &mut TxCache,
    ) -> Result<Arc<TxInfo>, ZcashError> {
        hash_bytes(txid)?;
        if let Some(info) = cache.get(txid) {
            return Ok(Arc::clone(info));
        }
        let result = self.node.get(Route::Tx(txid)).await?;
        let raw = result.get("hex").and_then(Value::as_str).ok_or(ZcashError::Protocol)?;
        let tx = sign::decode(&hex::decode(raw).map_err(|_| ZcashError::Protocol)?)?;
        if tx.txid().to_string() != txid || result.get("txid").and_then(Value::as_str) != Some(txid)
        {
            return Err(ZcashError::Protocol);
        }
        let confirmations =
            result.get("confirmations").and_then(Value::as_u64).ok_or(ZcashError::Protocol)?;
        let height = if confirmations == 0 {
            None
        } else {
            Some(
                result
                    .get("blockHeight")
                    .and_then(Value::as_u64)
                    .and_then(|height| u32::try_from(height).ok())
                    .filter(|height| *height > 0)
                    .ok_or(ZcashError::Protocol)?,
            )
        };
        let timestamp =
            result.get("blockTime").and_then(Value::as_i64).filter(|timestamp| *timestamp > 0);
        let info = Arc::new(TxInfo { tx, height, timestamp });
        cache.insert(txid.to_owned(), Arc::clone(&info));
        Ok(info)
    }

    pub(crate) async fn coins(&self, address: &str) -> Result<Vec<Coin>, ZcashError> {
        let rows: Vec<Unspent> = decode(self.node.get(Route::Unspent(address)).await?)?;
        if rows.len() > MAX_UNSPENT_ROWS {
            return Err(ZcashError::Protocol);
        }
        let owned_script = script(address)?;
        let mut cache = TxCache::new();
        let mut points = BTreeSet::new();
        let mut coins = Vec::with_capacity(rows.len());
        for row in rows {
            if !points.insert((row.txid.clone(), row.vout)) {
                return Err(ZcashError::Protocol);
            }
            let previous = self.tx(&row.txid, &mut cache).await?;
            let bundle = previous.tx.transparent_bundle().ok_or(ZcashError::Protocol)?;
            let output = bundle
                .vout
                .get(usize::try_from(row.vout).map_err(|_| ZcashError::Protocol)?)
                .ok_or(ZcashError::Protocol)?;
            let value_zat = decimal_zat(&row.value)?;
            let is_key_hash = output.script_pubkey() == &owned_script;
            if (!is_key_hash && !is_pay_to_pubkey(output, address))
                || u64::from(output.value()) != value_zat
            {
                return Err(ZcashError::Protocol);
            }
            // A transaction that spends a coinbase output can't have transparent outputs, and
            // every payment built here has one. A pay-to-pubkey output needs an unlocking
            // script this adapter never signs.
            if is_key_hash && !bundle.is_coinbase() && value_zat > 0 {
                coins.push(Coin {
                    txid: row.txid,
                    index: row.vout,
                    value_zat,
                    address: address.to_owned(),
                });
            }
        }
        Ok(coins)
    }

    pub(crate) async fn history_page(
        &self,
        address: &str,
        page: u32,
    ) -> Result<AddressPage, ZcashError> {
        let result: AddressPage = decode(self.node.get(Route::History { address, page }).await?)?;
        if result.address != address
            || result.page != page
            || result.total_pages > MAX_HISTORY_PAGES
        {
            return Err(ZcashError::Protocol);
        }
        result.balance_zat()?;
        for txid in &result.txids {
            hash_bytes(txid)?;
        }
        Ok(result)
    }

    /// Every page of the history of `address`, refused unless they are one snapshot that
    /// lists each of its transactions once.
    pub(crate) async fn history(&self, address: &str) -> Result<AddressPage, ZcashError> {
        let mut merged = self.history_page(address, 1).await?;
        for page in 2..=merged.total_pages {
            let next = self.history_page(address, page).await?;
            if next.total_pages != merged.total_pages || !next.is_same_snapshot(&merged) {
                return Err(ZcashError::Protocol);
            }
            merged.txids.extend(next.txids);
        }
        let unique: BTreeSet<&str> = merged.txids.iter().map(String::as_str).collect();
        let expected =
            merged.txs.checked_add(merged.unconfirmed_txs).ok_or(ZcashError::Protocol)?;
        if unique.len() != merged.txids.len() || u64::try_from(unique.len()).ok() != Some(expected)
        {
            return Err(ZcashError::Protocol);
        }
        Ok(merged)
    }

    /// Submits `raw_hex` and checks that Blockbook reports it as `txid`.
    pub(crate) async fn broadcast(&self, raw_hex: &str, txid: &str) -> Result<(), ZcashError> {
        if self.node.broadcast(raw_hex).await? != txid {
            return Err(ZcashError::Protocol);
        }
        Ok(())
    }
}
