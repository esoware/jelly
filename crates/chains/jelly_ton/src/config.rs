use jelly_core::ChainId;
use serde_json::Value;

pub(crate) const CHAIN: ChainId = ChainId::Ton;
// TON mainnet's zerostate root and file hashes, as `getMasterchainInfo` reports them.
const MAINNET_ZEROSTATE_ROOT_HASH: &str = "F6OpKZKqvqeFp6CQmFomXNMfMj2EnaUSOXN+Mh+wVWk=";
const MAINNET_ZEROSTATE_FILE_HASH: &str = "XplPz01CXAps5qeSWUtxcyBfdAo5zVb1N979KLSKD24=";
pub(crate) const MAX_FEE_NANOGRAM: u64 = 100_000_000;
/// The longest comment a payment may carry. This is Jelly's own limit, not the protocol's.
pub(crate) const MAX_COMMENT_BYTES: usize = 1024;

/// Reads a `getMasterchainInfo` result. `None` means it names no network.
pub(crate) fn is_mainnet(info: &Value) -> Option<bool> {
    let root = info.pointer("/init/root_hash").and_then(Value::as_str)?;
    let file = info.pointer("/init/file_hash").and_then(Value::as_str)?;
    Some(root == MAINNET_ZEROSTATE_ROOT_HASH && file == MAINNET_ZEROSTATE_FILE_HASH)
}
