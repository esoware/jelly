use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_net::{HttpEndpoint, HttpRequest, NetError, Outcome, Pool};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::config::CHAIN;
use crate::error::TonError;
use crate::method::Method;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_RESPONSE_BYTES: usize = 4_000_000;
// TON Center allows one unauthenticated request per second. Every TON endpoint is paced
// as if it were TON Center, and the spacing holds only because `PooledNode::gate`
// serializes calls. An identity check runs outside that gate.
const REQUEST_SPACING: Duration = Duration::from_millis(1_100);

#[async_trait]
pub trait TonNode: Send + Sync + fmt::Debug {
    /// Returns the bare `result` of a JSON-RPC call.
    async fn call(&self, method: Method, params: Value) -> Result<Value, TonError>;
    /// Sends a signed BOC with `sendBoc` in a single attempt.
    async fn broadcast(&self, boc: &str) -> Result<(), TonError>;
}

#[derive(Debug)]
pub(crate) struct PooledNode {
    pool: Arc<Pool<HttpEndpoint>>,
    // Serializes calls, so the pause in `request` spaces them a second apart.
    gate: Mutex<()>,
}

impl PooledNode {
    pub(crate) fn new(pool: Arc<Pool<HttpEndpoint>>) -> Result<Self, TonError> {
        if pool.chain() != CHAIN || pool.endpoints().is_empty() {
            return Err(TonError::Invalid("Configure a TON mainnet endpoint."));
        }
        Ok(Self { pool, gate: Mutex::new(()) })
    }
}

const fn classify(_: &NetError) -> Outcome {
    Outcome::Rotate
}

pub(crate) async fn request(
    endpoint: &HttpEndpoint,
    method: Method,
    params: &Value,
    timeout: Duration,
) -> Result<Value, NetError> {
    sleep(REQUEST_SPACING).await;
    let document =
        json!({"jsonrpc": "2.0", "id": 1, "method": method.as_str(), "params": params}).to_string();
    let body = endpoint
        .send("", HttpRequest::post_json(&document, timeout).with_max_bytes(MAX_RESPONSE_BYTES))
        .await?
        .into_body()?;
    let mut response: Value = serde_json::from_str(&body)
        .map_err(|_| NetError::protocol(endpoint.label(), "invalid TON response"))?;
    if response.get("ok").and_then(Value::as_bool) == Some(false) || response.get("error").is_some()
    {
        return Err(NetError::protocol(endpoint.label(), "TON request refused"));
    }
    response
        .get_mut("result")
        .map(Value::take)
        .filter(|result| !result.is_null())
        .ok_or_else(|| NetError::protocol(endpoint.label(), "missing TON result"))
}

#[async_trait]
impl TonNode for PooledNode {
    async fn call(&self, method: Method, params: Value) -> Result<Value, TonError> {
        let _guard = self.gate.lock().await;
        Ok(self
            .pool
            .call(method.as_str(), classify, async |endpoint: Arc<HttpEndpoint>| {
                request(&endpoint, method, &params, REQUEST_TIMEOUT).await
            })
            .await?)
    }

    async fn broadcast(&self, boc: &str) -> Result<(), TonError> {
        let _guard = self.gate.lock().await;
        let params = json!({"boc": boc});
        self.pool
            .broadcast_once(
                Method::SendBoc.as_str(),
                classify,
                async |endpoint: Arc<HttpEndpoint>| {
                    request(&endpoint, Method::SendBoc, &params, REQUEST_TIMEOUT).await
                },
            )
            .await?;
        Ok(())
    }
}
