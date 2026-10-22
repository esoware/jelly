//! The JSON-RPC shapes this build sends and reads.
//!
//! Answer types carry only the members the adapter uses, so an endpoint cannot
//! make the process allocate fields no caller reads. Unknown members are ignored,
//! because a node is free to add fields.

use alloy::primitives::{Address, B256, Bytes, U256};
use jelly_core::wire_enum;
use serde::{Deserialize, Serialize};

use crate::error::EvmError;
use crate::units;

wire_enum! {
    pub(crate) enum BlockTag {
        Latest => "latest",
        /// Includes the sender's executable mempool, so two sends in a row do not
        /// read the same nonce.
        Pending => "pending",
    }
}

/// The call object `eth_call` and `eth_estimateGas` take.
///
/// An estimate sets `from`, because some providers check the value against the
/// sender's balance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CallObject {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) from: Option<Address>,
    pub(crate) to: Address,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<U256>,
    #[serde(skip_serializing_if = "no_calldata")]
    pub(crate) input: Bytes,
}

/// A call with no calldata omits the member rather than sending `0x`, which some
/// providers read as a request to execute empty code.
fn no_calldata(input: &Bytes) -> bool {
    input.as_ref().is_empty()
}

impl CallObject {
    pub(crate) fn read(to: Address, data: Vec<u8>) -> Self {
        Self { from: None, to, value: None, input: Bytes::from(data) }
    }
}

/// One `eth_getLogs` filter.
///
/// `topics` is positional. Element zero is the event signature, one the indexed
/// sender and two the indexed recipient. A `null` element matches anything, and
/// an array element is an or-set, so one window covers every watched address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogFilter {
    #[serde(with = "alloy::serde::quantity")]
    pub(crate) from_block: u64,
    #[serde(with = "alloy::serde::quantity")]
    pub(crate) to_block: u64,
    pub(crate) address: Vec<Address>,
    pub(crate) topics: Vec<Option<Vec<B256>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LogEntry {
    pub(crate) address: Address,
    #[serde(default)]
    pub(crate) topics: Vec<B256>,
    #[serde(default)]
    pub(crate) data: Bytes,
    #[serde(default, with = "alloy::serde::quantity::opt")]
    pub(crate) block_number: Option<u64>,
    /// Served only by newer nodes. Without it a record carries its height and no
    /// time.
    #[serde(default, with = "alloy::serde::quantity::opt")]
    pub(crate) block_timestamp: Option<u64>,
    pub(crate) transaction_hash: Option<B256>,
    #[serde(default, with = "alloy::serde::quantity::opt")]
    pub(crate) log_index: Option<u64>,
    /// True when a reorg removed the log. Nothing may be recorded from one.
    #[serde(default)]
    pub(crate) removed: bool,
}

impl LogEntry {
    /// The identity a duplicate is recognised by. Both members are required,
    /// because defaulting either would collapse distinct transfers onto one key.
    pub(crate) const fn position(&self) -> Option<(B256, u64)> {
        match (self.transaction_hash, self.log_index) {
            (Some(hash), Some(index)) => Some((hash, index)),
            _ => None,
        }
    }
}

/// `status` and `effective_gas_price` are optional because nodes serving very
/// old receipts omit them. Absent means unknown, so a missing field never reports
/// a transaction as failed or free.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Receipt {
    pub(crate) transaction_hash: B256,
    #[serde(default, with = "alloy::serde::quantity::opt")]
    pub(crate) status: Option<u8>,
    #[serde(default, with = "alloy::serde::quantity::opt")]
    pub(crate) gas_used: Option<u64>,
    #[serde(default, with = "alloy::serde::quantity::opt")]
    pub(crate) effective_gas_price: Option<u128>,
    /// What the OP Stack charged to post the transaction's data to Ethereum, which
    /// the gas leaves out. Other networks omit it.
    #[serde(default, with = "alloy::serde::quantity::opt")]
    pub(crate) l1_fee: Option<u128>,
    #[serde(default, with = "alloy::serde::quantity::opt")]
    pub(crate) block_number: Option<u64>,
    /// Lets a later poll tell a reorged inclusion from one that stood.
    pub(crate) block_hash: Option<B256>,
}

impl Receipt {
    pub(crate) const fn landing(&self) -> Option<Landing> {
        match self.block_number {
            Some(height) => Some(Landing { height, block_hash: self.block_hash }),
            None => None,
        }
    }

    pub(crate) const fn execution(&self) -> Execution {
        match self.status {
            Some(1) => Execution::Succeeded,
            Some(_) => Execution::Reverted,
            None => Execution::Unknown,
        }
    }

    /// Present whether or not execution succeeded, because a reverted
    /// transaction still pays for the gas it burned.
    pub(crate) fn fee_wei(&self) -> Option<u128> {
        let used = self.gas_used?;
        let price = self.effective_gas_price?;
        price.checked_mul(u128::from(used))?.checked_add(self.l1_fee.unwrap_or(0))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Landing {
    pub(crate) height: u64,
    /// Absent only from a node that does not report it. A changed hash means the
    /// inclusion was reorged.
    pub(crate) block_hash: Option<B256>,
}

impl Landing {
    /// Without block hashes only heights can be compared, which is weaker but
    /// still catches a move.
    pub(crate) fn is_same(self, other: Self) -> bool {
        match (self.block_hash, other.block_hash) {
            (Some(left), Some(right)) => left == right,
            _ => self.height == other.height,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Execution {
    Succeeded,
    /// Per EIP-658, the transaction is on chain, consumed its nonce and paid a
    /// fee, but moved nothing.
    Reverted,
    Unknown,
}

/// `gasUsedRatio` is floating point and is not read, because no float may touch
/// the fee path.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FeeHistory {
    /// One entry per sampled block, plus a trailing prediction for the next
    /// block that some providers omit.
    #[serde(default)]
    pub(crate) base_fee_per_gas: Vec<U256>,
    /// Per block, the requested percentiles of the tips paid. Some providers
    /// omit it, and empty blocks give all-zero rows.
    pub(crate) reward: Option<Vec<Vec<U256>>>,
}

impl FeeHistory {
    /// The base fee of the next block as the node predicts it, or the newest one
    /// it did report.
    pub(crate) fn predicted_base_fee(&self) -> Result<u128, EvmError> {
        match self.base_fee_per_gas.last() {
            Some(value) => units::to_u128("base fee", *value),
            None => Ok(0),
        }
    }

    /// A row of zeros comes from an empty block and is dropped rather than
    /// counted, because a chain with two empty blocks did not become free. A zero
    /// in a busy block is a tip someone paid and stays, so every column samples
    /// the same blocks and a lower percentile never comes out above a higher one.
    pub(crate) fn column(&self, index: usize) -> Result<Vec<u128>, EvmError> {
        let Some(rows) = self.reward.as_ref() else {
            return Ok(Vec::new());
        };
        let mut values = Vec::with_capacity(rows.len());
        for row in rows {
            if row.iter().all(U256::is_zero) {
                continue;
            }
            let Some(value) = row.get(index) else { continue };
            values.push(units::to_u128("priority fee", *value)?);
        }
        Ok(values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_serialises_its_positional_topics_with_holes() {
        let owner: Address = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045".parse().expect("address");
        let filter = LogFilter {
            from_block: 10,
            to_block: 4_096,
            address: vec![owner],
            topics: vec![Some(vec![B256::ZERO]), None, Some(vec![B256::ZERO])],
        };
        let text = serde_json::to_string(&filter).expect("it serialises");
        assert!(text.contains(r#""fromBlock":"0xa""#), "{text}");
        assert!(text.contains(r#""toBlock":"0x1000""#), "{text}");
        assert!(text.contains(r#""topics":[["0x00"#), "{text}");
        assert!(text.contains("null"), "a hole is a null element: {text}");
    }

    #[test]
    fn a_call_object_omits_what_it_does_not_set() {
        let to: Address = "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045".parse().expect("address");
        let text = serde_json::to_string(&CallObject::read(to, Vec::new())).expect("serialises");
        assert!(!text.contains("from"), "{text}");
        assert!(!text.contains("value"), "{text}");
        assert!(!text.contains("input"), "{text}");
    }

    #[test]
    fn a_pending_receipt_is_a_null_answer_and_a_mined_one_carries_its_block() {
        assert_eq!(serde_json::from_str::<Option<Receipt>>("null").expect("null"), None);

        let body = r#"{
            "transactionHash":"0x2200000000000000000000000000000000000000000000000000000000000001",
            "status":"0x1","gasUsed":"0x5208","effectiveGasPrice":"0x4a817c800",
            "blockNumber":"0x112a881",
            "blockHash":"0xaa00000000000000000000000000000000000000000000000000000000000002",
            "logs":[],"transactionIndex":"0x2","type":"0x2"
        }"#;
        let receipt: Receipt = serde_json::from_str(body).expect("it decodes");
        assert_eq!(receipt.execution(), Execution::Succeeded);
        assert_eq!(receipt.fee_wei(), Some(21_000 * 20_000_000_000));
        let landing = receipt.landing().expect("a landing");
        assert_eq!(landing.height, 18_000_001);
        assert!(landing.block_hash.is_some());
    }

    /// Recorded from Base, where the L1 data fee is paid beside the gas.
    #[test]
    fn an_op_stack_receipt_counts_its_l1_data_fee_in_the_fee() {
        let body = r#"{
            "transactionHash":"0x1f1a0a2f5568b22f6c1b539336ebe1c09b471df2c2097506feaa7275b747f676",
            "status":"0x1","gasUsed":"0x3dcdb","effectiveGasPrice":"0xdce291f",
            "blockNumber":"0x32213ee",
            "blockHash":"0x068830ab3dd4246c572b4cf36055027572cdac1f812b84073305f242f27b0220",
            "l1GasPrice":"0x5ac004e","l1GasUsed":"0x6bf","l1Fee":"0x3cfa998f",
            "l1BaseFeeScalar":"0x8dd","l1BlobBaseFee":"0x56ffb1","l1BlobBaseFeeScalar":"0x101c12"
        }"#;
        let receipt: Receipt = serde_json::from_str(body).expect("it decodes");
        assert_eq!(receipt.l1_fee, Some(0x3cfa_998f));
        assert_eq!(receipt.fee_wei(), Some(0x3dcdb * 0xdce_291f + 0x3cfa_998f));
    }

    #[test]
    fn a_receipt_with_no_status_reports_an_unknown_execution_rather_than_a_failure() {
        let body = r#"{
            "transactionHash":"0x1111111111111111111111111111111111111111111111111111111111111111",
            "gasUsed":"0x5208"
        }"#;
        let receipt: Receipt = serde_json::from_str(body).expect("it decodes");
        assert_eq!(receipt.execution(), Execution::Unknown);
        assert_eq!(receipt.fee_wei(), None, "no gas price is no fee, not a free transaction");
        assert_eq!(receipt.landing(), None);
    }

    #[test]
    fn an_inclusion_is_compared_by_hash_when_both_sides_have_one() {
        let left = Landing { height: 10, block_hash: Some(B256::repeat_byte(1)) };
        let moved = Landing { height: 10, block_hash: Some(B256::repeat_byte(2)) };
        assert!(left.is_same(left));
        assert!(!left.is_same(moved), "the same height in another block is another inclusion");

        let bare = Landing { height: 10, block_hash: None };
        assert!(bare.is_same(Landing { height: 10, block_hash: Some(B256::ZERO) }));
        assert!(!bare.is_same(Landing { height: 11, block_hash: None }));
    }

    #[test]
    fn a_fee_history_reads_its_prediction_and_drops_the_zeros_empty_blocks_report() {
        let body = r#"{
            "oldestBlock":"0x1",
            "baseFeePerGas":["0x1","0x2","0x3","0x4"],
            "gasUsedRatio":[0.0,0.5,0.62],
            "reward":[["0x0","0x0"],["0x0","0x77359400"],["0x3b9aca00","0x77359400"]]
        }"#;
        let history: FeeHistory = serde_json::from_str(body).expect("it decodes");
        assert_eq!(history.predicted_base_fee().expect("a base fee"), 4);
        assert_eq!(
            history.column(0).expect("a column"),
            vec![0, 1_000_000_000],
            "the empty block is dropped and a busy block's zero tip is kept"
        );
        assert_eq!(history.column(1).expect("a column"), vec![2_000_000_000, 2_000_000_000]);
        assert!(history.column(7).expect("a missing column").is_empty());

        let bare: FeeHistory = serde_json::from_str("{}").expect("it decodes");
        assert_eq!(bare.predicted_base_fee().expect("zero"), 0);
        assert!(bare.column(0).expect("no rewards").is_empty());
    }

    #[test]
    fn a_log_with_no_position_is_not_dedupable_and_a_removed_one_is_flagged() {
        let body = r#"{
            "address":"0xdac17f958d2ee523a2206206994597c13d831ec7",
            "topics":[],"data":"0x","blockNumber":null,"transactionHash":null,
            "logIndex":null,"removed":true
        }"#;
        let entry: LogEntry = serde_json::from_str(body).expect("it decodes");
        assert_eq!(entry.position(), None);
        assert!(entry.removed);
        assert_eq!(entry.block_number, None);
    }
}
