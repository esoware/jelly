use jelly_core::ChainId;
use serde_json::Value;
use zcash_protocol::consensus::{BlockHeight, BranchId, MAIN_NETWORK};

pub(crate) const CHAIN: ChainId = ChainId::Zcash;
// At about 150 bytes for each P2PKH input, this keeps a payment under zcashd's 100,000 byte
// limit for a standard transaction.
pub(crate) const MAX_INPUTS: usize = 600;
// Bitcoin's P2PKH dust value, about ten times zcashd's 54 zatoshis, so a node with a relay
// policy stricter than zcashd's still relays the output.
pub(crate) const DUST_ZAT: u64 = 546;

pub(crate) fn is_mainnet_status(status: &Value) -> bool {
    status.pointer("/blockbook/coin").and_then(Value::as_str) == Some("Zcash")
        && status.pointer("/backend/chain").and_then(Value::as_str) == Some("main")
}

pub(crate) fn branch_at(height: u32) -> BranchId {
    BranchId::for_height(&MAIN_NETWORK, BlockHeight::from_u32(height))
}
