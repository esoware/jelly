use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use jelly_core::Amount;
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Value, json};
use tonlib_core::cell::{Cell, CellBuilder};
use tonlib_core::tlb_types::tlb::TLB as _;
use tonlib_core::types::TonAddress;
use tonlib_core::wallet::version_helper::VersionHelper;
use tonlib_core::wallet::versioned::v3::WalletDataV3;
use tonlib_core::wallet::versioned::v4::WalletDataV4;
use tonlib_core::wallet::versioned::v5::WalletDataV5;

use crate::address::non_bounceable;
use crate::config::{MAX_FEE_NANOGRAM, is_mainnet};
use crate::error::TonError;
use crate::message;
use crate::method::Method;
use crate::node::TonNode;
use crate::payload::Payment;
use crate::record::AccountTx;
use crate::wallet::{Version, ton_wallet};

const TRANSACTIONS_PAGE_LIMIT: u32 = 32;

#[derive(Debug)]
pub(crate) struct TonClient {
    node: Arc<dyn TonNode>,
}

/// A transaction's id. The sync cursor stores it in the node's own shape, `lt` as a
/// decimal string and `hash` as padded base64.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Position {
    #[serde(serialize_with = "serialize_lt", deserialize_with = "deserialize_lt")]
    pub(crate) lt: u64,
    #[serde(serialize_with = "serialize_hash", deserialize_with = "deserialize_hash")]
    pub(crate) hash: [u8; 32],
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's serialize_with passes the field by reference"
)]
fn serialize_lt<S: Serializer>(lt: &u64, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(lt)
}

fn deserialize_lt<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let text = String::deserialize(deserializer)?;
    decimal_u64(&text).map_err(|_| D::Error::custom("a logical time is not a decimal u64"))
}

fn serialize_hash<S: Serializer>(hash: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&STANDARD.encode(hash))
}

fn deserialize_hash<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
    let text = String::deserialize(deserializer)?;
    STANDARD
        .decode(text)
        .ok()
        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
        .ok_or_else(|| D::Error::custom("a transaction hash is not 32 bytes of base64"))
}

jelly_core::wire_enum! {
    pub(crate) enum AccountState {
        Active => "active",
        #[serde(alias = "uninit")]
        Uninitialized => "uninitialized",
        Frozen => "frozen",
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct Account {
    balance: String,
    code: String,
    data: String,
    pub(crate) state: AccountState,
    pub(crate) sync_utime: u32,
    pub(crate) last_transaction_id: Position,
}

/// The fields of an account that tell whether anything ever used it.
#[derive(Debug, Deserialize)]
struct Usage {
    balance: String,
    state: AccountState,
    last_transaction_id: Position,
}

impl Account {
    pub(crate) fn balance_nanogram(&self) -> Result<u64, TonError> {
        decimal_u64(&self.balance)
    }

    /// The next seqno of the wallet of `version` that `pubkey` controls, and whether the
    /// payment deploys it. A deployed account must hold that very contract.
    pub(crate) fn sequence(
        &self,
        pubkey: &[u8; 32],
        version: Version,
    ) -> Result<(u32, bool), TonError> {
        match self.state {
            AccountState::Uninitialized => Ok((0, true)),
            AccountState::Active => {
                let mismatch = TonError::Invalid(match version {
                    Version::V5r1 => "This TON account does not match an enabled V5R1 wallet.",
                    Version::V4r2 => "This TON account does not match a V4R2 wallet.",
                    Version::V3r2 => "This TON account does not match a V3R2 wallet.",
                });
                let wallet = ton_wallet(pubkey, version)?;
                let code = Cell::from_boc(&base64_bytes(&self.code)?)?;
                if code.cell_hash()
                    != VersionHelper::get_code(version.wallet_version())?.as_ref().cell_hash()
                {
                    return Err(mismatch);
                }
                let data = base64_bytes(&self.data)?;
                let read = match version {
                    Version::V5r1 => WalletDataV5::from_boc(&data).map(|data| {
                        (data.seqno, data.wallet_id, data.public_key, data.signature_allowed)
                    }),
                    Version::V4r2 => WalletDataV4::from_boc(&data)
                        .map(|data| (data.seqno, data.wallet_id, data.public_key, true)),
                    Version::V3r2 => WalletDataV3::from_boc(&data)
                        .map(|data| (data.seqno, data.wallet_id, data.public_key, true)),
                };
                let Ok((seqno, wallet_id, public_key, is_enabled)) = read else {
                    return Err(mismatch);
                };
                if public_key.as_slice() != pubkey || wallet_id != wallet.wallet_id || !is_enabled {
                    return Err(mismatch);
                }
                Ok((seqno, false))
            }
            AccountState::Frozen => {
                Err(TonError::Invalid("This TON wallet is frozen and cannot send from Jelly."))
            }
        }
    }
}

fn base64_bytes(text: &str) -> Result<Vec<u8>, TonError> {
    STANDARD.decode(text).map_err(|_| TonError::Protocol)
}

fn decimal_u64(text: &str) -> Result<u64, TonError> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(TonError::Protocol);
    }
    text.parse().map_err(|_| TonError::Protocol)
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, TonError> {
    serde_json::from_value(value).map_err(|_| TonError::Protocol)
}

/// A slice holding one address, the argument `get_wallet_address` takes.
fn address_slice(address: &TonAddress) -> Result<String, TonError> {
    let mut builder = CellBuilder::new();
    builder.store_address(address)?;
    Ok(STANDARD.encode(builder.build()?.to_boc(false)?))
}

/// The cell a get method returned in one stack entry.
fn stack_cell(entry: Option<&Value>) -> Result<Cell, TonError> {
    let entry = entry.and_then(Value::as_array).ok_or(TonError::Protocol)?;
    if !matches!(entry.first().and_then(Value::as_str), Some("cell" | "slice")) {
        return Err(TonError::Protocol);
    }
    let bytes = entry
        .get(1)
        .and_then(|object| object.get("bytes"))
        .and_then(Value::as_str)
        .ok_or(TonError::Protocol)?;
    Ok(Cell::from_boc(&base64_bytes(bytes)?)?)
}

fn stack_address(entry: Option<&Value>) -> Result<TonAddress, TonError> {
    Ok(stack_cell(entry)?.parser().load_address()?)
}

/// A non-negative number a get method returned in one stack entry, written as hex.
fn stack_number(entry: Option<&Value>) -> Result<u128, TonError> {
    let entry = entry.and_then(Value::as_array).ok_or(TonError::Protocol)?;
    if entry.first().and_then(Value::as_str) != Some("num") {
        return Err(TonError::Protocol);
    }
    let text = entry.get(1).and_then(Value::as_str).ok_or(TonError::Protocol)?;
    let digits = text.strip_prefix("0x").ok_or(TonError::Protocol)?;
    u128::from_str_radix(digits, 16).map_err(|_| TonError::Protocol)
}

impl TonClient {
    pub(crate) fn new(node: Arc<dyn TonNode>) -> Self {
        Self { node }
    }

    pub(crate) async fn tip(&self) -> Result<u64, TonError> {
        let result = self.node.call(Method::GetMasterchainInfo, json!({})).await?;
        if is_mainnet(&result) != Some(true) {
            return Err(TonError::Protocol);
        }
        result
            .pointer("/last/seqno")
            .and_then(Value::as_u64)
            .filter(|height| *height > 0)
            .ok_or(TonError::Protocol)
    }

    pub(crate) async fn account(
        &self,
        address: &TonAddress,
        tip: u64,
    ) -> Result<Account, TonError> {
        let params = json!({
            "address": non_bounceable(address),
            "seqno": tip,
        });
        let result = self.node.call(Method::GetAddressInformation, params).await?;
        if result.pointer("/block_id/seqno").and_then(Value::as_u64) != Some(tip) {
            return Err(TonError::Protocol);
        }
        decode(result)
    }

    /// Whether the account at `address` holds Gram, is anything but uninitialized, or
    /// has any history. One call, at the node's latest block.
    pub(crate) async fn is_used(&self, address: &TonAddress) -> Result<bool, TonError> {
        let params = json!({"address": non_bounceable(address)});
        let usage: Usage = decode(self.node.call(Method::GetAddressInformation, params).await?)?;
        Ok(decimal_u64(&usage.balance)? > 0
            || usage.state != AccountState::Uninitialized
            || usage.last_transaction_id.lt > 0)
    }

    /// Runs a get method at block `tip`, or at the node's latest block for `None`.
    async fn run(
        &self,
        address: &TonAddress,
        method: &'static str,
        stack: Value,
        tip: Option<u64>,
    ) -> Result<Vec<Value>, TonError> {
        let mut params = json!({
            "address": address.to_base64_url_flags(false, false),
            "method": method,
            "stack": stack,
        });
        if let Some(tip) = tip
            && let Some(object) = params.as_object_mut()
        {
            object.insert(String::from("seqno"), json!(tip));
        }
        let mut result = self.node.call(Method::RunGet, params).await?;
        let is_at_tip = tip.is_none_or(|tip| {
            result.pointer("/block_id/seqno").and_then(Value::as_u64) == Some(tip)
        });
        if !is_at_tip || result.get("exit_code").and_then(Value::as_i64) != Some(0) {
            return Err(TonError::Protocol);
        }
        match result.get_mut("stack").map(Value::take) {
            Some(Value::Array(stack)) => Ok(stack),
            _ => Err(TonError::Protocol),
        }
    }

    /// The jetton wallet `owner` holds under `master`, as the master computes it at block
    /// `tip`, or at the node's latest block for `None`.
    pub(crate) async fn jetton_wallet(
        &self,
        master: &TonAddress,
        owner: &TonAddress,
        tip: Option<u64>,
    ) -> Result<TonAddress, TonError> {
        let stack = json!([["tvm.Slice", address_slice(owner)?]]);
        let result = self.run(master, "get_wallet_address", stack, tip).await?;
        stack_address(result.first())
    }

    /// The balance of the jetton wallet at `wallet`, as its `get_wallet_data` reports it
    /// at block `tip`. A wallet that doesn't name `owner` and `master` in TEP-74's order
    /// is refused.
    pub(crate) async fn jetton_balance(
        &self,
        wallet: &TonAddress,
        owner: &TonAddress,
        master: &TonAddress,
        tip: u64,
    ) -> Result<Amount, TonError> {
        let result = self.run(wallet, "get_wallet_data", json!([]), Some(tip)).await?;
        let balance = stack_number(result.first())?;
        let named_owner = stack_address(result.get(1))?;
        let named_master = stack_address(result.get(2))?;
        if named_owner != *owner || named_master != *master {
            return Err(TonError::Protocol);
        }
        Ok(Amount::from_base_units(balance))
    }

    pub(crate) async fn fee_nanogram(&self, payment: &Payment) -> Result<u64, TonError> {
        let wallet = ton_wallet(&payment.pubkey, payment.version)?;
        let version = payment.version.wallet_version();
        let signed = VersionHelper::sign_msg(version, &message::body(payment)?, &[0; 64])?;
        let (init_code, init_data) = if payment.is_deploy {
            (
                STANDARD.encode(VersionHelper::get_code(version)?.to_boc(true)?),
                STANDARD.encode(
                    VersionHelper::get_data(version, &wallet.key_pair, wallet.wallet_id)?
                        .to_boc(true)?,
                ),
            )
        } else {
            (String::new(), String::new())
        };
        let params = json!({
            "address": payment.from,
            "body": STANDARD.encode(signed.to_boc(true)?),
            "init_code": init_code,
            "init_data": init_data,
            "ignore_chksig": true,
        });
        let result = self.node.call(Method::EstimateFee, params).await?;
        let fees = result.get("source_fees").ok_or(TonError::Protocol)?;
        let mut total_nanogram = 0u64;
        for field in ["in_fwd_fee", "storage_fee", "gas_fee", "fwd_fee"] {
            let value = fees.get(field).ok_or(TonError::Protocol)?;
            let fee_nanogram = match value.as_str() {
                Some(text) => decimal_u64(text)?,
                None => value.as_u64().ok_or(TonError::Protocol)?,
            };
            total_nanogram = total_nanogram.checked_add(fee_nanogram).ok_or(TonError::Protocol)?;
        }
        let estimate_nanogram = total_nanogram
            .checked_mul(12)
            .and_then(|fee| fee.checked_div(10))
            .and_then(|fee| fee.checked_add(1_000))
            .ok_or(TonError::Protocol)?;
        if total_nanogram == 0 || estimate_nanogram > MAX_FEE_NANOGRAM {
            return Err(TonError::Invalid(
                "The TON fee estimate is not reasonable. Try again later.",
            ));
        }
        Ok(estimate_nanogram)
    }

    /// A page of `address`'s history, newest first, from the transaction at `position`
    /// down. A node always holds the transaction it named, so an empty page is refused.
    pub(crate) async fn transactions(
        &self,
        address: &TonAddress,
        position: &Position,
    ) -> Result<Vec<AccountTx>, TonError> {
        let params = json!({
            "address": non_bounceable(address),
            "lt": position.lt.to_string(),
            "hash": STANDARD.encode(position.hash),
            "limit": TRANSACTIONS_PAGE_LIMIT,
            "archival": true,
        });
        let rows: Vec<AccountTx> = decode(self.node.call(Method::GetTransactions, params).await?)?;
        if rows.is_empty() {
            return Err(TonError::Protocol);
        }
        Ok(rows)
    }

    pub(crate) async fn broadcast(&self, boc: &str) -> Result<(), TonError> {
        self.node.broadcast(boc).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(text: &str) -> Vec<Value> {
        let result: Value = serde_json::from_str(text).unwrap();
        result.get("stack").and_then(Value::as_array).unwrap().clone()
    }

    #[test]
    fn real_get_method_results_read_as_the_wallet_and_its_data() {
        // toncenter's runGetMethod answers for Tether's USDT master and the jetton
        // wallet it names for UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_.
        let owner = TonAddress::from_base64_url("UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_")
            .unwrap();
        let master =
            TonAddress::from_base64_url("EQCxE6mUtQJKFnGfaROTKOt1lZbDiiX1kCixRv7Nw2Id_sDs")
                .unwrap();
        let wallet = fixture(include_str!("../tests/fixtures/usdt_wallet_address.json"));
        assert_eq!(
            stack_address(wallet.first()).unwrap(),
            TonAddress::from_base64_url("EQDg4AjfaxQBVsUFueenkKlHLhhYWrcBvCEzbEgfrT0nxuGC")
                .unwrap()
        );
        let data = fixture(include_str!("../tests/fixtures/usdt_wallet_data.json"));
        assert_eq!(stack_number(data.first()).unwrap(), 0);
        assert_eq!(stack_address(data.get(1)).unwrap(), owner);
        assert_eq!(stack_address(data.get(2)).unwrap(), master);
    }

    #[test]
    fn the_owner_argument_is_one_address_in_a_slice() {
        let owner = TonAddress::from_base64_url("UQCh41gQP1A4I0lnAn6yAfitDAIYpXG6UFIXqeSz1TVxNOJ_")
            .unwrap();
        let cell =
            Cell::from_boc(&STANDARD.decode(address_slice(&owner).unwrap()).unwrap()).unwrap();
        assert_eq!(cell.bit_len(), 267);
        assert_eq!(cell.parser().load_address().unwrap(), owner);
    }

    #[test]
    fn a_number_that_is_not_hex_or_is_negative_is_refused() {
        for text in ["-0x1", "12", "0xzz"] {
            stack_number(Some(&json!(["num", text]))).unwrap_err();
        }
        assert_eq!(stack_number(Some(&json!(["num", "0xd2"]))).unwrap(), 210);
        stack_number(Some(&json!(["cell", "0x1"]))).unwrap_err();
    }
}
