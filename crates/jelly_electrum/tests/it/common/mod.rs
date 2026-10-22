//! Every helper is total. A vector that does not parse becomes `null` and an
//! unresolved pointer becomes an empty value, so a broken fixture fails the
//! assertion that reads it.

use bitcoin::absolute::LockTime;
use bitcoin::transaction::Version;
use bitcoin::{ScriptBuf, Transaction};
use jelly_electrum::test_util::Answer;
use serde_json::Value;

pub(crate) const SERVER_VERSION: &str = include_str!("../../vectors/server_version.json");
pub(crate) const SERVER_VERSION_FAIL: &str = include_str!("../../vectors/server_version_fail.json");
pub(crate) const HISTORY: &str = include_str!("../../vectors/history.json");
pub(crate) const LISTUNSPENT: &str = include_str!("../../vectors/listunspent.json");
pub(crate) const BLOCK_HEADER: &str = include_str!("../../vectors/block_header.json");
pub(crate) const MEMPOOL_GET_INFO: &str = include_str!("../../vectors/mempool_get_info.json");
pub(crate) const ESTIMATEFEE: &str = include_str!("../../vectors/estimatefee.json");
pub(crate) const FEE_HISTOGRAM: &str = include_str!("../../vectors/fee_histogram.json");
pub(crate) const AUXPOW_HEADER: &str = include_str!("../../vectors/auxpow_header.json");

pub(crate) fn vector(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// The value at `pointer`, as the raw JSON a server would put in `result`.
pub(crate) fn raw(text: &str, pointer: &str) -> String {
    vector(text).pointer(pointer).map(ToString::to_string).unwrap_or_default()
}

pub(crate) fn text_at(value: &Value, pointer: &str) -> String {
    value.pointer(pointer).and_then(Value::as_str).unwrap_or_default().to_owned()
}

pub(crate) fn number_at(value: &Value, pointer: &str) -> u64 {
    value.pointer(pointer).and_then(Value::as_u64).unwrap_or_default()
}

pub(crate) fn numbers_at(value: &Value, pointer: &str) -> Vec<u64> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_u64).collect())
        .unwrap_or_default()
}

pub(crate) fn strings_at(value: &Value, pointer: &str) -> Vec<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect())
        .unwrap_or_default()
}

pub(crate) fn rows(value: &Value, pointer: &str) -> Vec<Value> {
    value.pointer(pointer).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// An error object when the case names one, and the verbatim `result` otherwise.
pub(crate) fn answer_of(case: &Value) -> Answer {
    match case.get("error") {
        Some(error) => Answer::Error {
            code: error.get("code").and_then(Value::as_i64).unwrap_or_default(),
            message: text_at(error, "/message"),
        },
        None => Answer::Result(case.get("result").map(ToString::to_string).unwrap_or_default()),
    }
}

pub(crate) fn quoted(body: &str) -> String {
    format!("\"{body}\"")
}

/// A `server.version` answer for protocol 1.4, so the version gates apply.
pub(crate) fn old_server_version() -> Answer {
    Answer::Result(r#"["ElectrumX 1.16.0","1.4"]"#.to_owned())
}

/// How many buckets the recorded histogram decodes into.
pub(crate) fn histogram_buckets() -> usize {
    rows(&vector(FEE_HISTOGRAM), "/expect").len()
}

pub(crate) const fn tx() -> Transaction {
    Transaction {
        version: Version::TWO,
        lock_time: LockTime::ZERO,
        input: Vec::new(),
        output: Vec::new(),
    }
}

/// The script of `1A1zP1eP5QGefi2DMPTfTL5SLmv7DivfNa`, whose scripthash the
/// protocol basics document publishes, which history.json records.
pub(crate) fn script() -> ScriptBuf {
    ScriptBuf::from_hex("76a91462e907b15cbf27d5425399ebf6f0fb50ebb88f1888ac").unwrap_or_default()
}

/// The header the vector file records at position `index`.
pub(crate) fn header_hex(index: usize) -> String {
    text_at(&vector(BLOCK_HEADER), &format!("/headers/{index}/hex"))
}

pub(crate) fn header_hash(index: usize) -> String {
    text_at(&vector(BLOCK_HEADER), &format!("/headers/{index}/hash"))
}
