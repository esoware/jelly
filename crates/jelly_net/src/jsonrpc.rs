use std::collections::HashMap;
use std::sync::Arc;

use jelly_core::EndpointLabel;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::value::RawValue;

use crate::error::NetError;
use crate::pool::Outcome;

const TARGET: &str = "jelly::net::jsonrpc";

/// A JSON-RPC 2.0 request document, serialized once before a rotation starts
/// and reused for every attempt of that walk.
#[derive(Debug, Clone)]
pub struct JsonRpcRequest {
    id: u64,
    text: Arc<str>,
}

/// Requests joined into one batch document. It keeps every request's id, so the answers
/// are matched against the requests that were actually sent.
#[derive(Debug, Clone)]
pub struct JsonRpcBatch {
    ids: Arc<[u64]>,
    text: Arc<str>,
}

#[derive(Debug, Serialize)]
struct RequestBody<'a, P> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: P,
}

impl JsonRpcRequest {
    /// `params` is any serializable value, so an HTTP call site never builds a
    /// `serde_json::Value`.
    pub fn new<P: Serialize>(id: u64, method: &str, params: &P) -> Result<Self, NetError> {
        let text = serde_json::to_string(&RequestBody { jsonrpc: "2.0", id, method, params })
            .map_err(|source| {
                NetError::config("a json-rpc request could not be serialized", source)
            })?;
        Ok(Self { id, text: Arc::from(text) })
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl JsonRpcBatch {
    /// Joins pre-serialized requests into one batch document.
    pub fn new(requests: &[JsonRpcRequest]) -> Self {
        let width =
            requests.iter().map(|request| request.as_str().len().saturating_add(1)).sum::<usize>();
        let mut text = String::with_capacity(width.saturating_add(2));
        text.push('[');
        for (index, request) in requests.iter().enumerate() {
            if index > 0 {
                text.push(',');
            }
            text.push_str(request.as_str());
        }
        text.push(']');
        Self { ids: requests.iter().map(|request| request.id).collect(), text: Arc::from(text) }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    pub(crate) fn ids(&self) -> &[u64] {
        &self.ids
    }
}

/// One error member as the server sent it, an object from most providers and a bare
/// string from some.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum JsonRpcError {
    Object { code: i64, message: String },
    Message(String),
}

impl JsonRpcError {
    /// A bare string carries no code, so it reads as 0.
    pub const fn code(&self) -> i64 {
        match *self {
            Self::Object { code, .. } => code,
            Self::Message(_) => 0,
        }
    }

    pub fn message(&self) -> &str {
        match *self {
            Self::Object { ref message, .. } | Self::Message(ref message) => message,
        }
    }

    /// Code 429 becomes [`NetError::RateLimited`]; other errors become [`NetError::Rpc`].
    fn into_error(self, label: &EndpointLabel) -> NetError {
        let code = self.code();
        if code == 429 {
            return NetError::RateLimited { endpoint: label.clone(), retry_after: None };
        }
        let (Self::Object { message, .. } | Self::Message(message)) = self;
        NetError::Rpc { endpoint: label.clone(), code, message }
    }
}

/// Responses and server pushes share this shape, and a push carries no `id`.
///
/// `R` is a `&RawValue` borrowed from an HTTP body, or a `Box<RawValue>` for a message
/// read off a socket, which does not outlive the read.
#[derive(Debug, Deserialize)]
#[serde(bound(deserialize = "R: Deserialize<'de>"))]
pub struct JsonRpcResponse<R> {
    pub id: Option<u64>,
    #[serde(default, deserialize_with = "present_result")]
    pub result: Option<R>,
    pub error: Option<JsonRpcError>,
}

/// Keeps a `result` of `null` distinct from an absent one.
///
/// A null result is an answer, as when `eth_getTransactionReceipt` reports a transaction
/// not yet mined. `Option`'s own deserializer folds null into `None`, which a malformed
/// response also produces, so the raw token is captured and the caller decodes it into
/// its own `Option`.
fn present_result<'de, D, R>(deserializer: D) -> Result<Option<R>, D::Error>
where
    D: Deserializer<'de>,
    R: Deserialize<'de>,
{
    R::deserialize(deserializer).map(Some)
}

/// An answer that does not parse. It is logged by position, because serde's message
/// quotes the answer back.
fn unparsed(label: &EndpointLabel, source: &serde_json::Error, detail: &'static str) -> NetError {
    tracing::debug!(
        target: TARGET,
        endpoint = %label,
        detail,
        category = ?source.classify(),
        line = source.line(),
        column = source.column(),
        "a json-rpc answer does not parse"
    );
    NetError::protocol(label, detail)
}

/// Decodes a single response straight into `T`.
pub fn json_rpc_decode<T: DeserializeOwned>(
    label: &EndpointLabel,
    body: &str,
) -> Result<T, NetError> {
    let envelope: JsonRpcResponse<&RawValue> = serde_json::from_str(body)
        .map_err(|source| unparsed(label, &source, "the answer is not a json-rpc response"))?;
    decode_result(label, envelope_result(label, envelope)?)
}

/// Decodes a `result` member, still unparsed, into the caller's own type.
pub(crate) fn decode_result<T: DeserializeOwned>(
    label: &EndpointLabel,
    raw: &RawValue,
) -> Result<T, NetError> {
    serde_json::from_str(raw.get()).map_err(|source| {
        unparsed(label, &source, "the result is not the shape that was asked for")
    })
}

/// The `result` members of a batch response, reordered to match `request_ids`.
///
/// The length is checked before any element is consumed and every element is
/// placed by its id, so a server that reorders or repeats cannot have an answer
/// read against the wrong request.
pub(crate) fn json_rpc_decode_batch<'a>(
    label: &EndpointLabel,
    body: &'a str,
    request_ids: &[u64],
) -> Result<Vec<&'a RawValue>, NetError> {
    let envelopes: Vec<JsonRpcResponse<&'a RawValue>> = serde_json::from_str(body)
        .map_err(|source| unparsed(label, &source, "the answer is not a json-rpc batch"))?;
    if envelopes.len() != request_ids.len() {
        tracing::debug!(
            target: TARGET,
            endpoint = %label,
            asked = request_ids.len(),
            answered = envelopes.len(),
            "a json-rpc batch was answered with a different count"
        );
        return Err(NetError::protocol(label, "the batch answer has a different count"));
    }
    let slots: HashMap<u64, usize> =
        request_ids.iter().enumerate().map(|(index, id)| (*id, index)).collect();
    let mut answers: Vec<Option<&'a RawValue>> = vec![None; request_ids.len()];
    for envelope in envelopes {
        let id = envelope.id.ok_or_else(|| NetError::protocol(label, "batch element has no id"))?;
        let slot = slots
            .get(&id)
            .copied()
            .ok_or_else(|| NetError::protocol(label, "batch element has an unrequested id"))?;
        let value = envelope_result(label, envelope)?;
        match answers.get_mut(slot) {
            Some(entry) if entry.is_none() => *entry = Some(value),
            Some(_) => return Err(NetError::protocol(label, "batch element id repeats")),
            None => return Err(NetError::protocol(label, "batch element id is out of range")),
        }
    }
    answers
        .into_iter()
        .collect::<Option<Vec<&'a RawValue>>>()
        .ok_or_else(|| NetError::protocol(label, "batch is missing an element"))
}

/// The answer one response carries, its `result` or its error.
pub(crate) fn envelope_result<R>(
    label: &EndpointLabel,
    envelope: JsonRpcResponse<R>,
) -> Result<R, NetError> {
    if let Some(error) = envelope.error {
        return Err(error.into_error(label));
    }
    envelope
        .result
        .ok_or_else(|| NetError::protocol(label, "response has neither result nor error"))
}

/// What a failover walk does with a JSON-RPC error code.
///
/// A malformed or unsupported request gets the same answer from every endpoint,
/// so rotating is wasted work and hides the real cause. Only the codes JSON-RPC 2.0
/// itself defines are read here, and a chain adds its own nodes' final codes.
pub const fn json_rpc_outcome(code: i64) -> Outcome {
    match code {
        -32700 | -32600 | -32601 | -32602 => Outcome::Fail,
        _ => Outcome::Rotate,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label() -> EndpointLabel {
        EndpointLabel::new("https", "example.com", None)
    }

    #[derive(Debug, PartialEq, Eq, serde::Deserialize)]
    struct Tip {
        height: u64,
    }

    #[test]
    fn a_batch_is_reordered_by_id() {
        let body = r#"[{"jsonrpc":"2.0","id":2,"result":22},{"jsonrpc":"2.0","id":1,"result":11}]"#;
        let answers = json_rpc_decode_batch(&label(), body, &[1, 2]).expect("two answers");
        assert_eq!(answers.first().map(|raw| raw.get()), Some("11"));
        assert_eq!(answers.get(1).map(|raw| raw.get()), Some("22"));
    }

    #[test]
    fn a_batch_that_does_not_match_the_request_is_refused() {
        for body in [
            r#"[{"jsonrpc":"2.0","id":1,"result":11}]"#,
            r#"[{"jsonrpc":"2.0","id":1,"result":11},{"jsonrpc":"2.0","id":1,"result":12}]"#,
            r#"[{"jsonrpc":"2.0","id":1,"result":11},{"jsonrpc":"2.0","id":9,"result":12}]"#,
            r#"[{"jsonrpc":"2.0","result":11},{"jsonrpc":"2.0","id":2,"result":12}]"#,
        ] {
            let error = json_rpc_decode_batch(&label(), body, &[1, 2]).expect_err(body);
            assert!(matches!(error, NetError::Protocol { .. }), "{body}");
        }
    }

    #[test]
    fn a_batch_remembers_the_id_of_every_request_in_it() {
        let first = JsonRpcRequest::new(4, "eth_chainId", &[(); 0]).expect("a request");
        let second = JsonRpcRequest::new(9, "eth_blockNumber", &[(); 0]).expect("a request");
        let batch = JsonRpcBatch::new(&[first, second]);
        assert_eq!(batch.ids(), [4, 9]);
    }

    #[test]
    fn a_null_result_is_an_answer_and_not_a_missing_member() {
        // `eth_getTransactionReceipt` answers null for a transaction not yet mined, so a
        // caller must be able to read one into its own `Option`.
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        assert_eq!(json_rpc_decode::<Option<Tip>>(&label(), body).expect("a decoded null"), None);

        let batch = r#"[{"jsonrpc":"2.0","id":1,"result":null}]"#;
        let answers = json_rpc_decode_batch(&label(), batch, &[1]).expect("one answer");
        assert_eq!(answers.first().map(|raw| raw.get()), Some("null"));
    }

    #[test]
    fn an_rpc_rate_limit_is_classified_for_single_and_batch_responses() {
        let body = r#"{"jsonrpc":"2.0","id":1,"error":{"code":429,"message":"Too many requests for a specific RPC call"}}"#;
        let single = json_rpc_decode::<u64>(&label(), body).expect_err("a rate limit");
        let batch = format!("[{body}]");
        let batched = json_rpc_decode_batch(&label(), &batch, &[1]).expect_err("a rate limit");
        for error in [single, batched] {
            assert!(matches!(error, NetError::RateLimited { retry_after: None, .. }), "{error}");
            assert_eq!(error.kind(), jelly_core::FailureKind::RateLimited);
            assert_eq!(error.label(), Some(&label()));
        }
    }
}
