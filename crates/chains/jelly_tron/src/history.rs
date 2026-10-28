//! Decodes the optional `/v1` indexer history, which a plain full node answers
//! with 404. A history failure doesn't stop balance sync, which uses the `/wallet`
//! routes.
//!
//! Non-transfer contracts and tokens we don't carry, unsolicited ones included,
//! are ignored, and so is a token transfer of zero. Anyone can make a TRC-20
//! contract log one between any two addresses: a zero `transferFrom` from the
//! victim to a lookalike of an address it pays plants the lookalike in its
//! history.
//!
//! The TRX a contract call moves comes from the internal transfers the indexer
//! lists with the transaction. A full node keeps those in a receipt only when
//! configured to, so we never read them from one.

use std::cmp::Ordering;

use jelly_core::{
    Amount, AssetId, TokenId, TxDetection, TxDirection, TxId, TxOutcome, TxRecord, TxRecordBuilder,
};
use serde::Deserialize;

use crate::abi;
use crate::address;
use crate::config::TRX;
use crate::pending::millis_to_secs;
use crate::raw::Contract;
use crate::shape::ContractResult;
use crate::token::token_at;

const TARGET: &str = "jelly::tron::history";

#[derive(Debug, Deserialize)]
pub(crate) struct Page<T> {
    // A bare `default` would make serde require `T: Default` of every row shape.
    #[serde(default = "Vec::new")]
    pub(crate) data: Vec<T>,
    #[serde(default)]
    meta: PageMeta,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PageMeta {
    #[serde(default)]
    fingerprint: Option<String>,
}

impl<T> Page<T> {
    /// The cursor for the next page, when there might be one.
    pub(crate) fn fingerprint(&self) -> Option<&str> {
        self.meta.fingerprint.as_deref()
    }
}

/// An indexer row from any stream, reduced to the txid and time the frontier
/// tracks.
pub(crate) trait Row {
    fn txid(&self) -> &str;
    fn timestamp_millis(&self) -> Option<i64>;
}

/// A row that makes whole records on its own. An internal transfer doesn't,
/// since it's one step of a larger transaction.
pub(crate) trait Recorded: Row {
    /// The block the row says it was mined in. `TronGrid`'s TRC-20 rows say none.
    fn height(&self) -> Option<u64>;
    /// The records before [`place`] puts them in a block.
    fn records(&self, address_id: i64, watched: &str) -> Vec<TxRecordBuilder>;
}

pub(crate) fn place(
    record: TxRecordBuilder,
    height: Option<u64>,
    timestamp_millis: Option<i64>,
    tip_height: u64,
) -> TxRecord {
    match height {
        Some(height) => {
            record.confirmed_at(height, timestamp_millis.map(millis_to_secs), tip_height).build()
        }
        None => match timestamp_millis {
            Some(seen) => record.seen_at(millis_to_secs(seen)).build(),
            None => record.build(),
        },
    }
}

/// One row from `/v1/accounts/{address}/transactions`, a whole transaction plus
/// the resource accounting and internal transfers the indexer adds to it.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct NativeRow {
    #[serde(rename = "txID", default)]
    txid: String,
    #[serde(default)]
    raw_data: NativeRawData,
    #[serde(rename = "blockNumber", default)]
    block_number: Option<u64>,
    #[serde(default)]
    block_timestamp: Option<i64>,
    #[serde(default)]
    ret: Vec<NativeRet>,
    #[serde(default)]
    net_fee: u64,
    #[serde(default)]
    energy_fee: u64,
    /// Every transfer made by contracts this transaction called, whoever they
    /// paid.
    #[serde(default)]
    internal_transactions: Vec<InternalRow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct NativeRawData {
    #[serde(default)]
    contract: Vec<NativeContract>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct NativeContract {
    #[serde(rename = "type", default)]
    contract_type: String,
    #[serde(default)]
    parameter: NativeParameter,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct NativeParameter {
    #[serde(default)]
    value: NativeValue,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct NativeValue {
    #[serde(default)]
    owner_address: Option<String>,
    #[serde(default)]
    to_address: Option<String>,
    #[serde(default)]
    amount: Option<u128>,
    #[serde(default)]
    contract_address: Option<String>,
    /// A contract call's calldata, hex.
    #[serde(default)]
    data: Option<String>,
    /// The TRX a contract call carried to the contract, in sun.
    #[serde(default)]
    call_value: Option<u128>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct NativeRet {
    /// TRX this transaction actually burned, in sun.
    #[serde(default)]
    fee: u64,
    #[serde(rename = "contractRet", default)]
    contract_ret: Option<String>,
}

impl NativeRow {
    /// Total TRX burned, in sun.
    ///
    /// `ret[0].fee` wins when present, since it covers both bandwidth and energy.
    /// The flattened fields are the fallback for indexers that omit it.
    fn fee_sun(&self) -> u64 {
        match self.ret.first() {
            Some(entry) if entry.fee > 0 => entry.fee,
            _ => self.net_fee.saturating_add(self.energy_fee),
        }
    }

    /// A row with no `contractRet` hasn't executed yet, which isn't the same as
    /// having failed.
    fn outcome(&self) -> TxOutcome {
        match self.ret.first().and_then(|entry| entry.contract_ret.as_deref()) {
            None => TxOutcome::Unknown,
            Some(text) if ContractResult::classify(text).is_success() => TxOutcome::Success,
            Some(_) => TxOutcome::Failed,
        }
    }

    /// The address that signed the transaction and paid its fee.
    pub(crate) fn sender(&self) -> Option<String> {
        self.raw_data.contract.first()?.parameter.value.owner_address.as_deref().and_then(canonical)
    }

    /// The token, recipient, and amount when this transaction calls `transfer`
    /// with a nonzero amount on a token we carry. A zero transfer is treated as a
    /// call that moves neither TRX nor a token.
    fn token_transfer(&self) -> Option<(TokenId, String, u128)> {
        let contract = self.raw_data.contract.first()?;
        if Contract::parse(&contract.contract_type)? != Contract::TriggerSmartContract {
            return None;
        }
        let value = &contract.parameter.value;
        let token =
            value.contract_address.as_deref().and_then(canonical).and_then(|at| token_at(&at))?;
        let calldata = hex::decode(value.data.as_deref()?).ok()?;
        // The VM ignores calldata past the two argument words, so a longer call
        // moves what those words say.
        let call = calldata.get(..abi::TRANSFER_CALL_BYTES)?;
        let (recipient, amount) = abi::decode_transfer(call).ok()?;
        (amount > 0).then(|| (token, address::display(&recipient), amount))
    }

    /// The TRC-20 stream reports the token movement without a fee, so the TRX the
    /// call burned is reported here, on a record with the same asset and txid that
    /// the store merges with that one.
    fn token_record(&self, address_id: i64, watched: &str) -> Option<TxRecordBuilder> {
        let (token, recipient, amount) = self.token_transfer()?;
        let (direction, counterparty) =
            direction(watched, self.sender().as_deref(), Some(recipient.as_str()))?;
        let outcome = self.outcome();
        // A failed transaction moved nothing but still cost its sender the fee.
        let moved = if outcome == TxOutcome::Failed { 0 } else { amount };
        let mut record = TxRecord::builder(
            AssetId::Token(token),
            address_id,
            TxId::new(TRX.chain(), &self.txid),
            direction,
            Amount::from_base_units(moved),
        )
        .outcome(outcome)
        .detection(TxDetection::Full);
        if let Some(other) = counterparty {
            record = record.counterparty(other);
        }
        // Only the sender pays, so a fee on an incoming transfer would be wrong
        // even though the indexer reports one for the transaction.
        if direction != TxDirection::Incoming {
            record = record.fee(Amount::from(self.fee_sun()));
        }
        Some(record)
    }

    /// All the TRX the transaction moved for one watched address, netted into one
    /// record: the transfer itself or what a call carried, plus every internal
    /// transfer to or from the address.
    ///
    /// The store keeps one row per address, asset, and txid, so every stream that
    /// reports this transaction reports it from here and none can overwrite the
    /// others with a partial amount.
    ///
    /// A failed transaction moved nothing, so its internal transfers are skipped,
    /// but it still cost the sender the fee. Only the sender pays, and a
    /// carried-token transfer's fee is already on that token's record.
    pub(crate) fn trx_record(&self, address_id: i64, watched: &str) -> Option<TxRecordBuilder> {
        let contract = self.raw_data.contract.first()?;
        let value = &contract.parameter.value;
        let sender = self.sender();
        let is_sender = sender.as_deref() == Some(watched);
        let outcome = self.outcome();
        let is_failed = outcome == TxOutcome::Failed;
        let carried = match Contract::parse(&contract.contract_type) {
            Some(Contract::Transfer) => {
                Some((value.to_address.as_deref(), value.amount.unwrap_or_default()))
            }
            Some(Contract::TriggerSmartContract) => value
                .call_value
                .filter(|sun| *sun > 0)
                .map(|sun| (value.contract_address.as_deref(), sun)),
            None => None,
        };
        let mut moved = Moved::default();
        if let Some((to, sun)) = carried {
            let sun = if is_failed { 0 } else { sun };
            moved.add(watched, sender, to.and_then(canonical), sun)?;
        }
        if !is_failed {
            for (from, to, sun) in
                self.internal_transactions.iter().filter_map(InternalRow::transfer)
            {
                moved.add(watched, from, to, sun)?;
            }
        }
        let (direction, amount, counterparty) = moved.net(watched)?;
        let mut record = TxRecord::builder(
            TRX,
            address_id,
            TxId::new(TRX.chain(), &self.txid),
            direction,
            Amount::from_base_units(amount),
        )
        .outcome(outcome)
        .detection(TxDetection::Full);
        if let Some(other) = counterparty {
            record = record.counterparty(other);
        }
        if is_sender && self.token_transfer().is_none() {
            record = record.fee(Amount::from(self.fee_sun()));
        }
        Some(record)
    }
}

impl Row for NativeRow {
    fn txid(&self) -> &str {
        &self.txid
    }

    fn timestamp_millis(&self) -> Option<i64> {
        self.block_timestamp
    }
}

impl Recorded for NativeRow {
    fn height(&self) -> Option<u64> {
        self.block_number
    }

    /// Only TRX and carried-token transfers become records. Staking, delegation,
    /// votes, and contract calls that move neither aren't transfers.
    fn records(&self, address_id: i64, watched: &str) -> Vec<TxRecordBuilder> {
        self.token_record(address_id, watched)
            .into_iter()
            .chain(self.trx_record(address_id, watched))
            .collect()
    }
}

/// Every TRX movement of one transaction for one watched address.
#[derive(Debug, Default)]
struct Moved {
    received: u128,
    sent: u128,
    to_self: u128,
    /// The first other party that paid the address.
    sender: Option<String>,
    /// The first other party the address paid.
    recipient: Option<String>,
    is_involved: bool,
}

impl Moved {
    /// Adds one movement if the address is on either side. `None` if a sum
    /// overflows `u128`.
    fn add(
        &mut self,
        watched: &str,
        from: Option<String>,
        to: Option<String>,
        sun: u128,
    ) -> Option<()> {
        let total = match (from.as_deref() == Some(watched), to.as_deref() == Some(watched)) {
            (true, true) => &mut self.to_self,
            (true, false) => {
                if self.recipient.is_none() {
                    self.recipient = to;
                }
                &mut self.sent
            }
            (false, true) => {
                if self.sender.is_none() {
                    self.sender = from;
                }
                &mut self.received
            }
            (false, false) => return Some(()),
        };
        *total = total.checked_add(sun)?;
        self.is_involved = true;
        Some(())
    }

    /// The net movement. More in than out is incoming and more out than in is
    /// outgoing, each by the difference. A net of zero keeps its direction when
    /// only one side had another party. What never left the address, or went out
    /// and came back, is a self-transfer.
    fn net(&self, watched: &str) -> Option<(TxDirection, u128, Option<String>)> {
        if !self.is_involved {
            return None;
        }
        match (self.received.cmp(&self.sent), self.sender.as_ref(), self.recipient.as_ref()) {
            (Ordering::Greater, sender, _) | (Ordering::Equal, sender @ Some(_), None) => Some((
                TxDirection::Incoming,
                self.received.checked_sub(self.sent)?,
                sender.cloned(),
            )),
            (Ordering::Less, _, recipient) | (Ordering::Equal, None, recipient @ Some(_)) => Some(
                (TxDirection::Outgoing, self.sent.checked_sub(self.received)?, recipient.cloned()),
            ),
            (Ordering::Equal, _, _) => Some((
                TxDirection::SelfTransfer,
                self.sent.checked_add(self.to_self)?,
                Some(watched.to_owned()),
            )),
        }
    }
}

/// One row from `/v1/accounts/{address}/internal-transactions`: a transfer a
/// contract made while a transaction ran.
///
/// A native row lists its own call's transfers in the same shape, minus the txid
/// and time.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct InternalRow {
    /// The transaction the transfer ran in.
    #[serde(default)]
    tx_id: String,
    #[serde(default)]
    from_address: Option<String>,
    #[serde(default)]
    to_address: Option<String>,
    #[serde(default)]
    data: InternalData,
    #[serde(default)]
    block_timestamp: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct InternalData {
    /// The kind of step that made the transfer.
    #[serde(default)]
    note: Option<String>,
    /// True when the step reverted, so it moved nothing.
    #[serde(default)]
    rejected: bool,
    #[serde(default)]
    call_value: CallValue,
}

/// What a step moved, keyed by asset. `_` is TRX in sun, and every other key is a
/// TRC-10 token, which we don't carry.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct CallValue {
    #[serde(rename = "_", default)]
    trx_sun: Option<u128>,
}

impl InternalRow {
    /// The TRX this step moved, with its two sides. A rejected step moved
    /// nothing, and only a call, a contract creation, or a self-destruct pays its
    /// recipient. The other notes name staking and voting steps a contract took
    /// for itself.
    fn transfer(&self) -> Option<(Option<String>, Option<String>, u128)> {
        if self.data.rejected || !is_payment(self.data.note.as_deref()) {
            return None;
        }
        let sun = self.data.call_value.trx_sun.filter(|sun| *sun > 0)?;
        let from = self.from_address.as_deref().and_then(canonical);
        let to = self.to_address.as_deref().and_then(canonical);
        Some((from, to, sun))
    }

    /// Whether this step moved TRX to or from `watched`, which makes its
    /// transaction worth reading.
    pub(crate) fn pays(&self, watched: &str) -> bool {
        self.transfer().is_some_and(|(from, to, _)| {
            from.as_deref() == Some(watched) || to.as_deref() == Some(watched)
        })
    }
}

/// The indexer documents the note as plain text but java-tron writes it hex
/// encoded, so we read both. A missing note counts as a call, which nearly every
/// transfer is.
fn is_payment(note: Option<&str>) -> bool {
    let Some(note) = note else {
        return true;
    };
    let decoded = hex::decode(note).ok().and_then(|bytes| String::from_utf8(bytes).ok());
    matches!(decoded.as_deref().unwrap_or(note), "call" | "create" | "suicide")
}

impl Row for InternalRow {
    fn txid(&self) -> &str {
        &self.tx_id
    }

    fn timestamp_millis(&self) -> Option<i64> {
        self.block_timestamp
    }
}

/// One row from `/v1/accounts/{address}/transactions/trc20`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TokenRow {
    #[serde(default)]
    transaction_id: String,
    #[serde(default)]
    token_info: TokenInfo,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(rename = "type", default)]
    event_type: Option<String>,
    /// A decimal string, because a TRC-20 value can overflow a JSON number.
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    block_timestamp: Option<i64>,
    #[serde(default, alias = "blockNumber")]
    block_number: Option<u64>,
}

/// Only the contract address is read. The scale comes from the compiled token
/// table, not the row's `decimals`.
#[derive(Debug, Clone, Default, Deserialize)]
struct TokenInfo {
    #[serde(default)]
    address: Option<String>,
}

impl TokenRow {
    /// True unless the row names an event that isn't a transfer.
    fn is_transfer(&self) -> bool {
        self.event_type.as_deref().unwrap_or("Transfer").eq_ignore_ascii_case("Transfer")
    }

    /// The token this row is about, if we carry it.
    fn token(&self) -> Option<TokenId> {
        token_at(&canonical(self.token_info.address.as_deref()?)?)
    }

    /// The amount in base units. A value that doesn't parse is a fault, never
    /// zero.
    fn amount(&self) -> Option<u128> {
        self.value.as_deref()?.parse::<u128>().ok()
    }

    fn record(&self, address_id: i64, watched: &str) -> Option<TxRecordBuilder> {
        if !self.is_transfer() {
            return None;
        }
        let token = self.token()?;
        let Some(amount) = self.amount() else {
            tracing::warn!(
                target: TARGET,
                txid = %self.transaction_id,
                "skipped a token transfer whose value doesn't parse"
            );
            return None;
        };
        if amount == 0 {
            return None;
        }
        let from = self.from.as_deref().and_then(canonical);
        let to = self.to.as_deref().and_then(canonical);
        let (direction, counterparty) = direction(watched, from.as_deref(), to.as_deref())?;

        let mut record = TxRecord::builder(
            AssetId::Token(token),
            address_id,
            TxId::new(TRX.chain(), &self.transaction_id),
            direction,
            Amount::from_base_units(amount),
        )
        .detection(TxDetection::Full);
        if let Some(other) = counterparty {
            record = record.counterparty(other);
        }
        // This route reports token movement only. The TRX burned is on the
        // record the native stream reports for the same txid.
        Some(record)
    }
}

impl Row for TokenRow {
    fn txid(&self) -> &str {
        &self.transaction_id
    }

    fn timestamp_millis(&self) -> Option<i64> {
        self.block_timestamp
    }
}

impl Recorded for TokenRow {
    fn height(&self) -> Option<u64> {
        self.block_number
    }

    fn records(&self, address_id: i64, watched: &str) -> Vec<TxRecordBuilder> {
        self.record(address_id, watched).into_iter().collect()
    }
}

fn canonical(value: &str) -> Option<String> {
    address::canonical("history row", value).ok()
}

fn direction(
    watched: &str,
    from: Option<&str>,
    to: Option<&str>,
) -> Option<(TxDirection, Option<String>)> {
    match (from == Some(watched), to == Some(watched)) {
        (true, true) => Some((TxDirection::SelfTransfer, to.map(ToOwned::to_owned))),
        (true, false) => Some((TxDirection::Outgoing, to.map(ToOwned::to_owned))),
        (false, true) => Some((TxDirection::Incoming, from.map(ToOwned::to_owned))),
        (false, false) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{TRX_OWNER, TRX_OWNER_HEX, TRX_RECIPIENT, TRX_RECIPIENT_HEX, USDT};

    fn native(body: &str) -> NativeRow {
        serde_json::from_str(body).expect("a native row decodes")
    }

    fn token(body: &str) -> TokenRow {
        serde_json::from_str(body).expect("a token row decodes")
    }

    /// The first record placed in the block the row itself names, as a walk does
    /// when the row names one.
    fn placed(
        row: &impl Recorded,
        address_id: i64,
        watched: &str,
        tip_height: u64,
    ) -> Option<TxRecord> {
        let record = row.records(address_id, watched).into_iter().next()?;
        Some(place(record, row.height(), row.timestamp_millis(), tip_height))
    }

    /// A call from [`TRX_OWNER`] to the USDT contract, with [`TRX_RECIPIENT`] and
    /// five USDT as its two argument words.
    fn token_call(selector: &str, trailing: &str, contract_ret: &str) -> NativeRow {
        token_call_of(
            selector,
            "00000000000000000000000000000000000000000000000000000000004c4b40",
            trailing,
            contract_ret,
        )
    }

    fn token_call_of(
        selector: &str,
        amount_word: &str,
        trailing: &str,
        contract_ret: &str,
    ) -> NativeRow {
        let peer_word = "000000000000000000000000df0c86c531205eb7a4e27d9589ba7188dfb32f08";
        native(&format!(
            r#"{{"txID":"jj","blockNumber":100,"block_timestamp":1786585806000,
                "ret":[{{"contractRet":"{contract_ret}","fee":13026600}}],
                "raw_data":{{"contract":[{{"type":"TriggerSmartContract","parameter":{{"value":{{
                  "data":"{selector}{peer_word}{amount_word}{trailing}",
                  "owner_address":"{TRX_OWNER_HEX}",
                  "contract_address":"41a614f803b6fd780986a42c78ec9c7f77e6ded13c"}}}}}}]}}}}"#
        ))
    }

    fn transfer_row(amount: u128, contract_ret: &str, fee: u64) -> NativeRow {
        native(&format!(
            r#"{{"txID":"aa","blockNumber":100,"block_timestamp":1786585806000,
                "ret":[{{"contractRet":"{contract_ret}","fee":{fee}}}],
                "raw_data":{{"contract":[{{"type":"TransferContract","parameter":{{"value":{{
                  "amount":{amount},"owner_address":"{TRX_OWNER_HEX}","to_address":"{TRX_RECIPIENT_HEX}"}}}}}}]}}}}"#
        ))
    }

    #[test]
    fn a_native_transfer_classifies_direction_amount_and_fee() {
        let row = transfer_row(7, "SUCCESS", 0);
        let sent = placed(&row, 4, TRX_OWNER, 100).expect("a record");
        assert_eq!(sent.direction, TxDirection::Outgoing);
        assert_eq!(sent.outcome, TxOutcome::Success);
        assert_eq!(sent.amount, Amount::from_base_units(7));
        assert_eq!(sent.counterparty.as_deref(), Some(TRX_RECIPIENT));
        assert_eq!(sent.fee, Some(Amount::ZERO), "the sender pays, and this one paid nothing");
        assert_eq!(sent.height, Some(100));
        assert_eq!(sent.timestamp, Some(1_786_585_806), "milliseconds become seconds");
        assert_eq!(sent.confirmations, 1);
        assert_eq!(sent.detection, TxDetection::Full);

        let received = placed(&row, 9, TRX_RECIPIENT, 100).expect("a record");
        assert_eq!(received.direction, TxDirection::Incoming);
        assert_eq!(received.fee, None, "a recipient never paid the fee");
        assert_eq!(received.counterparty.as_deref(), Some(TRX_OWNER));
    }

    #[test]
    fn a_failed_transaction_moves_nothing_and_still_costs_its_fee() {
        let row = transfer_row(500, "OUT_OF_ENERGY", 345_000);
        let record = placed(&row, 4, TRX_OWNER, 100).expect("a record");
        assert_eq!(record.outcome, TxOutcome::Failed);
        assert_eq!(record.amount, Amount::ZERO);
        assert_eq!(record.fee, Some(Amount::from_base_units(345_000)));
    }

    #[test]
    fn a_row_that_is_not_a_transfer_or_not_ours_is_not_a_record() {
        let staking = native(&format!(
            r#"{{"txID":"bb","raw_data":{{"contract":[{{"type":"FreezeBalanceV2Contract",
                "parameter":{{"value":{{"owner_address":"{TRX_OWNER_HEX}"}}}}}}]}}}}"#
        ));
        assert!(placed(&staking, 4, TRX_OWNER, 1).is_none());

        let elsewhere = transfer_row(7, "SUCCESS", 0);
        assert!(
            placed(&elsewhere, 4, USDT, 1).is_none(),
            "a transfer between two other addresses is not this address's history"
        );

        let empty = native(r#"{"txID":"cc","raw_data":{"contract":[]}}"#);
        assert!(placed(&empty, 4, TRX_OWNER, 1).is_none());
    }

    #[test]
    fn a_row_that_has_not_executed_yet_is_unknown_rather_than_failed() {
        let unmined = native(&format!(
            r#"{{"txID":"dd","block_timestamp":1786585806000,
                "raw_data":{{"contract":[{{"type":"TransferContract","parameter":{{"value":{{
                  "amount":9,"owner_address":"{TRX_OWNER_HEX}","to_address":"{TRX_RECIPIENT_HEX}"}}}}}}]}}}}"#
        ));
        let record = placed(&unmined, 4, TRX_OWNER, 100).expect("a record");
        assert_eq!(record.outcome, TxOutcome::Unknown);
        assert_eq!(record.amount, Amount::from_base_units(9), "unknown is not failed");
        assert_eq!(record.height, None);
        assert_eq!(record.confirmations, 0);
        assert_eq!(record.timestamp, Some(1_786_585_806));
    }

    #[test]
    fn a_token_send_on_the_native_stream_carries_the_trx_it_burned() {
        let sent =
            placed(&token_call("a9059cbb", "", "SUCCESS"), 4, TRX_OWNER, 100).expect("a record");
        assert_eq!(sent.asset, AssetId::Token(TokenId::UsdtTrc20));
        assert_eq!(sent.direction, TxDirection::Outgoing);
        assert_eq!(sent.amount, Amount::from_base_units(5_000_000));
        assert_eq!(sent.counterparty.as_deref(), Some(TRX_RECIPIENT));
        assert_eq!(sent.fee, Some(Amount::from_base_units(13_026_600)));
        assert_eq!(sent.height, Some(100));

        let longer =
            placed(&token_call("a9059cbb", "ff", "SUCCESS"), 4, TRX_OWNER, 100).expect("a record");
        assert_eq!(longer.amount, Amount::from_base_units(5_000_000), "trailing bytes are ignored");

        let reverted =
            placed(&token_call("a9059cbb", "", "REVERT"), 4, TRX_OWNER, 100).expect("a record");
        assert_eq!(reverted.outcome, TxOutcome::Failed);
        assert_eq!(reverted.amount, Amount::ZERO, "a reverted send moved nothing");
        assert_eq!(reverted.fee, Some(Amount::from_base_units(13_026_600)));

        let received = placed(&token_call("a9059cbb", "", "SUCCESS"), 9, TRX_RECIPIENT, 100)
            .expect("a record");
        assert_eq!(received.fee, None, "a recipient never paid the fee");

        assert!(
            placed(&token_call("095ea7b3", "", "SUCCESS"), 4, TRX_OWNER, 100).is_none(),
            "an approval is not a transfer"
        );
    }

    #[test]
    fn a_token_transfer_uses_the_compiled_scale_and_only_the_tokens_this_wallet_carries() {
        let row = token(&format!(
            r#"{{"transaction_id":"ee","token_info":{{"address":"{USDT}","decimals":2}},
                "from":"{TRX_OWNER}","to":"{TRX_RECIPIENT}","type":"Transfer","value":"125000000",
                "block_number":100,"block_timestamp":1786585806000}}"#
        ));
        let record = placed(&row, 4, TRX_OWNER, 100).expect("a record");
        assert_eq!(record.asset, AssetId::Token(TokenId::UsdtTrc20));
        assert_eq!(record.amount, Amount::from_base_units(125_000_000));
        assert_eq!(record.fee, None, "the TRX burned is on the native record");
        assert_eq!(record.height, Some(100));

        let airdrop = token(&format!(
            r#"{{"transaction_id":"ff","token_info":{{"address":"{TRX_RECIPIENT}"}},
                "from":"{TRX_OWNER}","to":"{TRX_RECIPIENT}","type":"Transfer","value":"1"}}"#
        ));
        assert!(
            placed(&airdrop, 4, TRX_OWNER, 1).is_none(),
            "an airdropped token never reaches the interface"
        );
    }

    #[test]
    fn a_token_row_that_is_not_a_transfer_or_whose_value_does_not_parse_is_dropped() {
        let approval = token(&format!(
            r#"{{"transaction_id":"gg","token_info":{{"address":"{USDT}"}},
                "from":"{TRX_OWNER}","to":"{TRX_RECIPIENT}","type":"Approval","value":"1"}}"#
        ));
        assert!(placed(&approval, 4, TRX_OWNER, 1).is_none());

        let exponent = token(&format!(
            r#"{{"transaction_id":"hh","token_info":{{"address":"{USDT}"}},
                "from":"{TRX_OWNER}","to":"{TRX_RECIPIENT}","type":"Transfer","value":"1.5e9"}}"#
        ));
        assert!(
            placed(&exponent, 4, TRX_OWNER, 1).is_none(),
            "a value that does not parse is never coerced to zero"
        );
    }

    /// A third party's `transferFrom(owner, lookalike, 0)` reaches the TRC-20
    /// stream as the owner sending nothing, and a stranger's transfer of nothing as
    /// the owner receiving it.
    #[test]
    fn a_token_transfer_of_nothing_is_never_recorded_from_either_stream() {
        for (from, to) in [(TRX_OWNER, TRX_RECIPIENT), (TRX_RECIPIENT, TRX_OWNER)] {
            let row = token(&format!(
                r#"{{"transaction_id":"mm","token_info":{{"address":"{USDT}"}},
                    "from":"{from}","to":"{to}","type":"Transfer","value":"0"}}"#
            ));
            assert!(row.records(4, TRX_OWNER).is_empty(), "{from} to {to}");
        }

        let zero = "0000000000000000000000000000000000000000000000000000000000000000";
        for watched in [TRX_OWNER, TRX_RECIPIENT] {
            assert!(
                token_call_of("a9059cbb", zero, "", "SUCCESS").records(4, watched).is_empty(),
                "a call that moves nothing is not a transfer for {watched}"
            );
        }
    }

    #[test]
    fn a_transfer_to_the_watched_address_itself_is_a_self_transfer() {
        let row = token(&format!(
            r#"{{"transaction_id":"ii","token_info":{{"address":"{USDT}"}},
                "from":"{TRX_OWNER}","to":"{TRX_OWNER}","type":"Transfer","value":"5"}}"#
        ));
        let record = placed(&row, 4, TRX_OWNER, 1).expect("a record");
        assert_eq!(record.direction, TxDirection::SelfTransfer);
    }

    #[test]
    fn an_empty_page_still_carries_its_fingerprint_and_a_bare_one_carries_none() {
        let page: Page<TokenRow> =
            serde_json::from_str(r#"{"data":[],"meta":{"fingerprint":"Fp1"}}"#).expect("a page");
        assert_eq!(page.fingerprint(), Some("Fp1"));
        assert!(page.data.is_empty());

        let bare: Page<TokenRow> = serde_json::from_str("{}").expect("a page");
        assert_eq!(bare.fingerprint(), None);
    }

    fn step(note: &str, is_rejected: bool, call_value: &str) -> InternalRow {
        serde_json::from_str(&format!(
            r#"{{"internal_tx_id":"kk","tx_id":"ll","block_timestamp":1786585806000,
                "from_address":"{TRX_RECIPIENT_HEX}","to_address":"{TRX_OWNER_HEX}",
                "data":{{"note":"{note}","rejected":{is_rejected},"call_value":{call_value}}}}}"#
        ))
        .expect("an internal row decodes")
    }

    #[test]
    fn only_trx_a_call_actually_paid_makes_an_internal_row_worth_reading() {
        assert!(step("call", false, r#"{"_":5}"#).pays(TRX_OWNER));
        assert!(
            step("call", false, r#"{"_":5}"#).pays(TRX_RECIPIENT),
            "the paying side counts too"
        );
        assert!(!step("call", false, r#"{"_":5}"#).pays(USDT), "a transfer between others");
        assert!(step("63616c6c", false, r#"{"_":5}"#).pays(TRX_OWNER), "a hex encoded note");
        assert!(step("suicide", false, r#"{"_":5}"#).pays(TRX_OWNER));
        assert!(!step("call", true, r#"{"_":5}"#).pays(TRX_OWNER), "a rejected step moved nothing");
        assert!(!step("call", false, r#"{"_":0}"#).pays(TRX_OWNER));
        assert!(!step("call", false, r#"{"1002000":5}"#).pays(TRX_OWNER), "a TRC-10 token");
        assert!(
            !step("delegateResourceOfEnergy", false, r#"{"_":5}"#).pays(TRX_OWNER),
            "a delegation lends resources and pays no one"
        );
        let row = step("call", false, r#"{"_":5}"#);
        assert_eq!((row.txid(), row.timestamp_millis()), ("ll", Some(1_786_585_806_000)));
    }
}
