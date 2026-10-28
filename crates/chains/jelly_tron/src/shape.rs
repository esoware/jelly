//! Decodes `/wallet` JSON. Protobuf omits default values, so a missing number
//! reads as zero. Whether an account exists is tracked separately, since
//! activating a missing account costs extra TRX.

use jelly_core::wire_enum;
use serde::Deserialize;

use crate::error::TronError;
use crate::node::Answered;

/// The TAPOS reference a transaction pins itself to, plus the network's own
/// clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BlockRef {
    pub(crate) number: u64,
    pub(crate) block_id: [u8; 32],
    /// Head block time in milliseconds. Expiry is judged by the network's clock,
    /// not ours, so timing starts here.
    pub(crate) timestamp_millis: i64,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct NowBlock {
    #[serde(rename = "blockID", default)]
    block_id: String,
    #[serde(default)]
    block_header: BlockHeader,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BlockHeader {
    #[serde(default)]
    raw_data: BlockHeaderRaw,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct BlockHeaderRaw {
    #[serde(default)]
    number: u64,
    #[serde(default)]
    timestamp: i64,
}

impl NowBlock {
    pub(crate) fn block_ref(&self, answer: &Answered) -> Result<BlockRef, TronError> {
        let raw = hex::decode(&self.block_id).map_err(|_| answer.unusable("block id"))?;
        let block_id: [u8; 32] =
            raw.as_slice().try_into().map_err(|_| answer.unusable("block id"))?;
        Ok(BlockRef {
            number: self.block_header.raw_data.number,
            block_id,
            timestamp_millis: self.block_header.raw_data.timestamp,
        })
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Account {
    #[serde(default)]
    address: Option<String>,
    #[serde(default)]
    balance: u64,
    #[serde(default)]
    create_time: Option<i64>,
}

impl Account {
    pub(crate) const fn balance_sun(&self) -> u64 {
        self.balance
    }

    /// False when the address has no account at all, which makes a transfer to it
    /// an activation.
    pub(crate) const fn is_activated(&self) -> bool {
        self.address.is_some() || self.create_time.is_some() || self.balance > 0
    }
}

/// One account's spendable resource pools.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
pub(crate) struct Resources {
    /// The daily free allowance, refilled over a rolling twenty-four hours.
    #[serde(rename = "freeNetLimit", default)]
    free_net_limit: u64,
    #[serde(rename = "freeNetUsed", default)]
    free_net_used: u64,
    /// Bandwidth from staked TRX.
    #[serde(rename = "NetLimit", default)]
    net_limit: u64,
    #[serde(rename = "NetUsed", default)]
    net_used: u64,
    #[serde(rename = "EnergyLimit", default)]
    energy_limit: u64,
    #[serde(rename = "EnergyUsed", default)]
    energy_used: u64,
}

impl Resources {
    pub(crate) const fn free_bandwidth(&self) -> u64 {
        self.free_net_limit.saturating_sub(self.free_net_used)
    }

    pub(crate) const fn staked_bandwidth(&self) -> u64 {
        self.net_limit.saturating_sub(self.net_used)
    }

    pub(crate) const fn energy(&self) -> u64 {
        self.energy_limit.saturating_sub(self.energy_used)
    }
}

/// The governance parameters, a flat array of named values.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ChainParameters {
    #[serde(rename = "chainParameter", default)]
    parameters: Vec<ChainParameter>,
}

#[derive(Debug, Clone, Deserialize)]
struct ChainParameter {
    key: String,
    /// An unset parameter carries no value at all.
    #[serde(default)]
    value: Option<i64>,
}

impl ChainParameters {
    pub(crate) fn get(&self, key: &str) -> Option<i64> {
        self.parameters.iter().find(|entry| entry.key == key).and_then(|entry| entry.value)
    }
}

/// What `triggerconstantcontract` answered: the return words, the energy the call
/// would use, and whether it executed cleanly.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ConstantCall {
    #[serde(default)]
    constant_result: Vec<String>,
    /// Total energy the call would use, surcharge included.
    #[serde(default)]
    energy_used: u64,
    /// The dynamic-energy surcharge inside `energy_used`, charged on contracts
    /// whose recent usage is above the network threshold.
    #[serde(default)]
    energy_penalty: u64,
    #[serde(default)]
    result: CallResult,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct CallResult {
    #[serde(default)]
    result: bool,
    #[serde(default)]
    message: Option<String>,
}

impl ConstantCall {
    pub(crate) const fn energy_used(&self) -> u64 {
        self.energy_used
    }

    pub(crate) const fn energy_penalty(&self) -> u64 {
        self.energy_penalty
    }

    /// The revert reason, if the call didn't execute cleanly.
    ///
    /// `result.result` only says the request was well formed and stays true on a
    /// revert, so the message is the only failure signal.
    pub(crate) fn failure(&self) -> Option<String> {
        let message = self.result.message.as_deref().unwrap_or_default();
        if self.result.result && message.is_empty() {
            return None;
        }
        if message.is_empty() {
            return Some(String::from("no reason given"));
        }
        Some(read_message(message))
    }

    pub(crate) fn first_word(&self) -> Option<&str> {
        self.constant_result.first().map(String::as_str)
    }
}

/// TRON returns `message` as hex for a contract revert and as plain text for a
/// validation failure. We read both, and anything that doesn't decode to printable
/// text is kept as it arrived.
pub(crate) fn read_message(message: &str) -> String {
    let trimmed = message.trim();
    if trimmed.len() >= 8
        && let Ok(bytes) = hex::decode(trimmed)
        && let Ok(text) = String::from_utf8(bytes)
        && text.chars().all(|c| !c.is_control() || c == '\n')
        && text.chars().any(char::is_alphabetic)
    {
        return text;
    }
    trimmed.to_owned()
}

wire_enum! {
    /// A transaction's result, as TRON reports it in `ret.contractRet` and in a
    /// receipt's `receipt.result`.
    ///
    /// Mirrors `Transaction.Result.contractResult`, and a test pins the table
    /// against the vendored enum. Any other word is
    /// [`ContractResult::Unclassified`], and only [`ContractResult::Success`]
    /// counts as success.
    pub(crate) enum ContractResult {
        Default => "DEFAULT",
        Success => "SUCCESS",
        Revert => "REVERT",
        BadJumpDestination => "BAD_JUMP_DESTINATION",
        OutOfMemory => "OUT_OF_MEMORY",
        PrecompiledContract => "PRECOMPILED_CONTRACT",
        StackTooSmall => "STACK_TOO_SMALL",
        StackTooLarge => "STACK_TOO_LARGE",
        IllegalOperation => "ILLEGAL_OPERATION",
        StackOverflow => "STACK_OVERFLOW",
        OutOfEnergy => "OUT_OF_ENERGY",
        OutOfTime => "OUT_OF_TIME",
        JvmStackOverFlow => "JVM_STACK_OVER_FLOW",
        Unknown => "UNKNOWN",
        TransferFailed => "TRANSFER_FAILED",
        InvalidCode => "INVALID_CODE",
        Unclassified => "UNCLASSIFIED",
    }
}

impl ContractResult {
    /// Reads one `contractRet` or `receipt.result` string.
    pub(crate) fn classify(text: &str) -> Self {
        Self::parse(text.trim().to_ascii_uppercase().as_str()).unwrap_or(Self::Unclassified)
    }

    pub(crate) const fn is_success(self) -> bool {
        matches!(self, Self::Success)
    }
}

/// A mined transaction's receipt.
///
/// `gettransactioninfobyid` answers a bare `{}` for an unmined transaction, so the
/// block number is the only signal read and the echoed `id` is ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TransactionInfo {
    #[serde(rename = "blockNumber", default)]
    block_number: Option<u64>,
    #[serde(rename = "blockTimeStamp", default)]
    block_timestamp: Option<i64>,
    /// Total TRX burned, in sun. Absent means nothing was burned.
    #[serde(default)]
    fee: u64,
    #[serde(default)]
    receipt: Receipt,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct Receipt {
    #[serde(default)]
    result: Option<String>,
}

impl TransactionInfo {
    pub(crate) const fn height(&self) -> Option<u64> {
        self.block_number
    }

    pub(crate) const fn timestamp_millis(&self) -> Option<i64> {
        self.block_timestamp
    }

    pub(crate) const fn fee_sun(&self) -> u64 {
        self.fee
    }

    /// A native transfer has no receipt result, so an empty one on a transaction
    /// that reached a block is a success.
    pub(crate) fn result(&self) -> ContractResult {
        self.receipt.result.as_deref().map_or(ContractResult::Success, ContractResult::classify)
    }
}

/// Enough of `gettransactionbyid` to know whether the network holds the
/// transaction at all, pool included.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct TransactionRow {
    #[serde(rename = "txID", default)]
    txid: Option<String>,
}

impl TransactionRow {
    pub(crate) const fn is_found(&self) -> bool {
        self.txid.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::answered;
    use crate::proto::protocol::transaction::result::ContractResult as VendoredResult;
    use crate::route::Route;

    #[test]
    fn a_message_is_read_whether_it_is_hex_or_plain_words() {
        assert_eq!(
            read_message(&hex::encode("balance is not sufficient")),
            "balance is not sufficient"
        );
        assert_eq!(read_message("Contract validate error"), "Contract validate error");
        assert_eq!(read_message(" 0011 "), "0011", "hex that is not text stays as it arrived");
        assert_eq!(read_message("abc"), "abc", "odd length is not hex");
    }

    #[test]
    fn a_head_block_becomes_a_tapos_reference() {
        let answer = answered(
            Route::NowBlock,
            r#"{"blockID":"000000000515a651af7c721ebc0c55d84f973b989508d8f40ba10affa980fd3e",
                "block_header":{"raw_data":{"number":85304913,"timestamp":1786585806000}}}"#,
        );
        let block: NowBlock = answer.decode().expect("it decodes");
        let reference = block.block_ref(&answer).expect("a reference");
        assert_eq!(reference.number, 85_304_913);
        assert_eq!(reference.timestamp_millis, 1_786_585_806_000);
        assert_eq!(
            hex::encode(reference.block_id),
            "000000000515a651af7c721ebc0c55d84f973b989508d8f40ba10affa980fd3e"
        );
    }

    #[test]
    fn a_block_id_that_is_not_thirty_two_bytes_is_unusable_rather_than_guessed_at() {
        let answer = answered(Route::NowBlock, r#"{"blockID":"00ff"}"#);
        let block: NowBlock = answer.decode().expect("it decodes");
        let error = block.block_ref(&answer).expect_err("four bytes is not a block id");
        assert!(matches!(error, TronError::Shape { what: "block id", .. }), "{error:?}");
    }

    #[test]
    fn a_never_activated_account_is_told_apart_from_an_empty_one() {
        let absent: Account = answered(Route::Account, "{}").decode().expect("it decodes");
        assert_eq!((absent.balance_sun(), absent.is_activated()), (0, false));

        let empty_but_real: Account = answered(Route::Account, r#"{"address":"41ff","balance":0}"#)
            .decode()
            .expect("decodes");
        assert_eq!((empty_but_real.balance_sun(), empty_but_real.is_activated()), (0, true));

        let funded: Account =
            answered(Route::Account, r#"{"balance":7}"#).decode().expect("it decodes");
        assert_eq!((funded.balance_sun(), funded.is_activated()), (7, true));
    }

    #[test]
    fn an_omitted_resource_number_is_zero_and_a_pool_never_goes_negative() {
        let fresh: Resources =
            answered(Route::AccountResource, r#"{"freeNetLimit":600}"#).decode().expect("decodes");
        assert_eq!(fresh.free_bandwidth(), 600);
        assert_eq!(fresh.staked_bandwidth(), 0);
        assert_eq!(fresh.energy(), 0);

        let spent: Resources = answered(
            Route::AccountResource,
            r#"{"freeNetLimit":600,"freeNetUsed":900,"NetLimit":10,"NetUsed":4,"EnergyLimit":19}"#,
        )
        .decode()
        .expect("decodes");
        assert_eq!(spent.free_bandwidth(), 0, "used above the limit is empty, never negative");
        assert_eq!(spent.staked_bandwidth(), 6);
        assert_eq!(spent.energy(), 19);
    }

    #[test]
    fn a_constant_call_that_reverted_is_a_failure_even_though_the_request_was_fine() {
        let ok: ConstantCall = answered(
            Route::ConstantContract,
            r#"{"constant_result":["07"],"result":{"result":true},"energy_used":9,"energy_penalty":2}"#,
        )
        .decode()
        .expect("decodes");
        assert_eq!(ok.failure(), None);
        assert_eq!(ok.first_word(), Some("07"));
        assert_eq!((ok.energy_used(), ok.energy_penalty()), (9, 2));

        let reverted: ConstantCall = answered(
            Route::ConstantContract,
            r#"{"result":{"result":true,"message":"524556455254206f70636f6465206578656375746564"}}"#,
        )
        .decode()
        .expect("decodes");
        assert_eq!(reverted.failure().as_deref(), Some("REVERT opcode executed"));

        let bare: ConstantCall =
            answered(Route::ConstantContract, r#"{"result":{"result":false}}"#)
                .decode()
                .expect("decodes");
        assert_eq!(bare.failure().as_deref(), Some("no reason given"));
    }

    #[test]
    fn a_receipt_without_a_result_is_a_native_transfer_that_succeeded() {
        let native: TransactionInfo = answered(
            Route::TransactionInfoById,
            r#"{"id":"aa","blockNumber":7,"blockTimeStamp":1,"receipt":{"net_usage":265}}"#,
        )
        .decode()
        .expect("decodes");
        assert_eq!(native.height(), Some(7));
        assert_eq!(native.result(), ContractResult::Success);
        assert_eq!(native.fee_sun(), 0);

        let burnt: TransactionInfo = answered(
            Route::TransactionInfoById,
            r#"{"id":"bb","blockNumber":9,"fee":345000,"receipt":{"result":"OUT_OF_ENERGY"}}"#,
        )
        .decode()
        .expect("decodes");
        assert_eq!(burnt.result(), ContractResult::OutOfEnergy);
        assert!(!burnt.result().is_success());
        assert_eq!(burnt.fee_sun(), 345_000);

        let unmined: TransactionInfo =
            answered(Route::TransactionInfoById, "{}").decode().expect("decodes");
        assert_eq!(unmined.height(), None, "a transaction that is not mined has no block");
    }

    #[test]
    fn the_contract_result_table_still_agrees_with_the_vendored_protos() {
        let vendored: Vec<VendoredResult> = (0..=i32::from(u8::MAX))
            .filter_map(|value| VendoredResult::try_from(value).ok())
            .collect();
        assert_eq!(
            vendored.len().saturating_add(1),
            ContractResult::COUNT,
            "every vendored outcome, and one for a word none of them spells"
        );
        for outcome in vendored {
            let name = outcome.as_str_name();
            assert_eq!(ContractResult::parse(name).map(ContractResult::as_str), Some(name));
        }
        assert_eq!(VendoredResult::from_str_name(ContractResult::Unclassified.as_str()), None);
    }

    #[test]
    fn every_contract_result_has_one_spelling_and_only_success_succeeds() {
        for result in ContractResult::ALL {
            assert_eq!(ContractResult::parse(result.as_str()), Some(result), "{result}");
            assert_eq!(result.is_success(), result == ContractResult::Success, "{result}");
        }
        assert_eq!(ContractResult::classify(" success "), ContractResult::Success);
        assert_eq!(
            ContractResult::classify("SOMETHING_THE_VM_ADDED"),
            ContractResult::Unclassified
        );
    }

    #[test]
    fn a_transaction_the_network_has_never_seen_is_an_empty_row() {
        let missing: TransactionRow =
            answered(Route::TransactionById, "{}").decode().expect("decodes");
        assert!(!missing.is_found());
        let held: TransactionRow =
            answered(Route::TransactionById, r#"{"txID":"aa"}"#).decode().expect("decodes");
        assert!(held.is_found());
    }
}
