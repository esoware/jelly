use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_net::{HttpEndpoint, HttpRequest, NetError, Outcome, Pool};
use serde_json::{Value, json};

use crate::config::CHAIN;
use crate::error::XrpError;
use crate::method::Method;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RESPONSE_BYTES: usize = 4_000_000;

#[async_trait]
pub trait XrpNode: Send + Sync + fmt::Debug {
    /// Returns the bare `result` object of a JSON-RPC call. An `actNotFound` from
    /// `account_info` or a `txnNotFound` from `tx` comes back as `Ok`, because callers read
    /// it as an answer, while `account_tx` always answers with a history.
    async fn call(&self, method: Method, params: Value) -> Result<Value, XrpError>;
    /// Submits a signed blob in a single attempt and returns the submit result, which
    /// carries `engine_result`.
    async fn broadcast(&self, blob: &str) -> Result<Value, XrpError>;
}

#[derive(Debug)]
pub(crate) struct PooledNode {
    pool: Arc<Pool<HttpEndpoint>>,
}

impl PooledNode {
    pub(crate) fn new(pool: Arc<Pool<HttpEndpoint>>) -> Result<Self, XrpError> {
        if pool.chain() != CHAIN || pool.endpoints().is_empty() {
            return Err(XrpError::Invalid("Configure an XRP mainnet endpoint."));
        }
        Ok(Self { pool })
    }
}

const fn classify(_: &NetError) -> Outcome {
    Outcome::Rotate
}

pub(crate) fn covers_range(result: &Value, first_ledger: u64, last_ledger: u64) -> bool {
    result.get("validated").and_then(Value::as_bool) == Some(true)
        && result
            .get("ledger_index_min")
            .and_then(Value::as_u64)
            .is_some_and(|first| first <= first_ledger)
        && result.get("ledger_index_max").and_then(Value::as_u64) == Some(last_ledger)
}

pub(crate) async fn request(
    endpoint: &HttpEndpoint,
    method: Method,
    params: &Value,
    timeout: Duration,
) -> Result<Value, NetError> {
    let document = json!({"method": method.as_str(), "params": [params]}).to_string();
    let body = endpoint
        .send("", HttpRequest::post_json(&document, timeout).with_max_bytes(MAX_RESPONSE_BYTES))
        .await?
        .into_body()?;
    let response: Value = serde_json::from_str(&body)
        .map_err(|_| NetError::protocol(endpoint.label(), "invalid XRP response"))?;
    answer(method, params, response).map_err(|detail| NetError::protocol(endpoint.label(), detail))
}

/// The `result` of a response, or why the server failed to give one.
fn answer(method: Method, params: &Value, mut response: Value) -> Result<Value, &'static str> {
    let result = response.get_mut("result").ok_or("missing XRP result")?.take();
    if !result.is_object() {
        return Err("invalid XRP result");
    }
    if let Some(error) = result.get("error").and_then(Value::as_str)
        && !matches!(error, "actNotFound" | "txnNotFound")
    {
        return Err("XRP request refused");
    }
    // `XrpClient::history` checks the range again for any `XrpNode`. Checking it here as
    // well makes a server without the full history a transport failure, so the pool
    // rotates to another. That includes one answering `actNotFound`, because history
    // outlives an account: rippled and Clio both list a deleted account's transactions,
    // and nothing for one that never existed.
    if method == Method::AccountTx {
        let wanted = params
            .get("ledger_index_min")
            .and_then(Value::as_u64)
            .zip(params.get("ledger_index_max").and_then(Value::as_u64));
        if wanted.is_none_or(|(first, last)| !covers_range(&result, first, last)) {
            return Err("incomplete XRP ledger history");
        }
    }
    Ok(result)
}

#[async_trait]
impl XrpNode for PooledNode {
    async fn call(&self, method: Method, params: Value) -> Result<Value, XrpError> {
        Ok(self
            .pool
            .call(method.as_str(), classify, async |endpoint: Arc<HttpEndpoint>| {
                request(&endpoint, method, &params, REQUEST_TIMEOUT).await
            })
            .await?)
    }

    async fn broadcast(&self, blob: &str) -> Result<Value, XrpError> {
        let params = json!({"tx_blob": blob, "fail_hard": false});
        Ok(self
            .pool
            .broadcast_once(
                Method::Submit.as_str(),
                classify,
                async |endpoint: Arc<HttpEndpoint>| {
                    request(&endpoint, Method::Submit, &params, REQUEST_TIMEOUT).await
                },
            )
            .await?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DELETED: &str = "rf2c74F1Z2BrvL1dV7WMHYo6Jyaw446Fre";

    fn history_params() -> Value {
        json!({"account": DELETED, "ledger_index_min": 32_570, "ledger_index_max": 107_774_530})
    }

    /// Clio's answers for the account `AccountDelete`
    /// 1AF19BF9717DA0B05A3BFC5007873E7743BA54C0311CCCCC60776AAEAC5C4635 removed from mainnet.
    #[test]
    fn a_deleted_account_is_missing_but_its_history_is_still_served() {
        let info = json!({"result": {"account": DELETED, "error": "actNotFound", "error_code": 19,
            "error_message": "Account not found.", "ledger_index": 107_774_530,
            "validated": true, "status": "error"}});
        let read = answer(Method::AccountInfo, &json!({"account": DELETED}), info).unwrap();
        assert_eq!(read.get("error").and_then(Value::as_str), Some("actNotFound"));

        let history = json!({"result": {"account": DELETED, "ledger_index_max": 107_774_530,
            "ledger_index_min": 32_570, "limit": 10, "transactions": [], "validated": true,
            "status": "success"}});
        answer(Method::AccountTx, &history_params(), history).unwrap();
    }

    #[test]
    fn a_server_that_will_not_list_a_missing_accounts_history_is_rotated_away_from() {
        let refused = json!({"result": {"account": DELETED, "error": "actNotFound",
            "status": "error", "validated": true}});
        assert_eq!(
            answer(Method::AccountTx, &history_params(), refused),
            Err("incomplete XRP ledger history")
        );
    }
}
