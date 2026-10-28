use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::Transport;
use jelly_net::{HttpEndpoint, HttpRequest, NetError, Outcome, Pool};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::config::CHAIN;
use crate::error::ZcashError;
use crate::route::Route;

const READ: &str = "Zcash read";
const BROADCAST: &str = "Zcash broadcast";
// The pool cuts a Blockbook attempt off at 15 seconds anyway.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
// Room for a transaction document, which holds the hex of a transaction up to
// `sign::MAX_TX_BYTES`.
const MAX_RESPONSE_BYTES: usize = 4_500_000;
// Public Blockbook servers limit how fast one client reads. Zelcore's refuses with HTTP 429
// after about forty reads half a second apart, and keeps up with about thirty a minute.
const READ_SPACING: Duration = Duration::from_secs(2);
// An endpoint cooling down after a failure admits no attempt, so a call waits for one.
const ADMISSION_WAIT: Duration = Duration::from_millis(200);
// With `ADMISSION_WAIT`, a call waits up to 20 seconds for an endpoint to admit an attempt.
const ADMISSION_RETRIES: u32 = 100;

#[async_trait]
pub trait ZcashNode: Send + Sync + fmt::Debug {
    /// Reads `route`, and fails when the response holds an `error` key.
    async fn get(&self, route: Route<'_>) -> Result<Value, ZcashError>;
    /// Submits the transaction in one attempt that is never repeated, and returns the txid
    /// Blockbook reports for it.
    async fn broadcast(&self, raw_hex: &str) -> Result<String, ZcashError>;
}

#[derive(Debug)]
pub(crate) struct PooledNode {
    pool: Arc<Pool<HttpEndpoint>>,
    // Serializes calls, so the pause in `get` spaces reads for the rate limits of public
    // Blockbook servers. A broadcast takes the gate without the pause, and an identity check
    // runs outside it.
    gate: Mutex<()>,
}

impl PooledNode {
    pub(crate) fn new(pool: Arc<Pool<HttpEndpoint>>) -> Result<Self, ZcashError> {
        if pool.chain() != CHAIN
            || pool.endpoints().is_empty()
            || pool.endpoints().iter().any(|endpoint| endpoint.transport() != Transport::Blockbook)
        {
            return Err(ZcashError::Invalid("Configure a Zcash mainnet Blockbook endpoint."));
        }
        Ok(Self { pool, gate: Mutex::new(()) })
    }
}

const fn classify(_: &NetError) -> Outcome {
    Outcome::Rotate
}

pub(crate) async fn read(
    endpoint: &HttpEndpoint,
    route: Route<'_>,
    timeout: Duration,
) -> Result<Value, NetError> {
    let (path, query) = route.path_and_query();
    request(endpoint, &path, &query, HttpRequest::get(timeout)).await
}

async fn request(
    endpoint: &HttpEndpoint,
    path: &str,
    query: &str,
    request: HttpRequest<'_>,
) -> Result<Value, NetError> {
    let body = endpoint
        .send_query(path, query, request.with_max_bytes(MAX_RESPONSE_BYTES))
        .await?
        .into_body()?;
    let value: Value = serde_json::from_str(&body)
        .map_err(|_| NetError::protocol(endpoint.label(), "invalid Zcash response"))?;
    if value.get("error").is_some() {
        return Err(NetError::protocol(endpoint.label(), "Zcash request refused"));
    }
    Ok(value)
}

// `NoneReady` means no endpoint admitted an attempt, so no request left, and retrying even
// a `broadcast_once` call can't submit a transaction twice.
async fn retry_until_admitted<T, F, Fut>(
    operation: &'static str,
    mut call: F,
) -> Result<T, NetError>
where
    T: Send,
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<T, NetError>> + Send,
{
    for _ in 0..ADMISSION_RETRIES {
        let result = call().await;
        if !matches!(result, Err(NetError::NoneReady { .. })) {
            return result;
        }
        sleep(ADMISSION_WAIT).await;
    }
    Err(NetError::NoneReady { chain: CHAIN, operation })
}

#[async_trait]
impl ZcashNode for PooledNode {
    async fn get(&self, route: Route<'_>) -> Result<Value, ZcashError> {
        let _guard = self.gate.lock().await;
        sleep(READ_SPACING).await;
        Ok(retry_until_admitted(READ, || {
            self.pool.call(READ, classify, async |endpoint: Arc<HttpEndpoint>| {
                read(&endpoint, route, REQUEST_TIMEOUT).await
            })
        })
        .await?)
    }

    async fn broadcast(&self, raw_hex: &str) -> Result<String, ZcashError> {
        let _guard = self.gate.lock().await;
        let response = retry_until_admitted(BROADCAST, || {
            self.pool.broadcast_once(BROADCAST, classify, async |endpoint: Arc<HttpEndpoint>| {
                let body =
                    HttpRequest::post_bytes("text/plain", raw_hex.as_bytes(), REQUEST_TIMEOUT);
                request(&endpoint, "api/v2/sendtx/", "", body).await
            })
        })
        .await?;
        response
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or(ZcashError::Protocol)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jelly_core::{
        Amount, AssetId, ChainAdapter as _, Clock as _, SendAmount, SystemClock,
        test_util::{
            FakeEventSink, MemoryCursorStore, MemoryPinStore, TxRequestBuilder,
            WatchedAddressBuilder,
        },
    };
    use jelly_net::{HttpConnector, Network};
    use zcash_primitives::transaction::Transaction;
    use zcash_protocol::consensus::BranchId;

    use super::*;
    use crate::adapter::ZcashAdapter;
    use crate::entry::{ENTRY, SPEC};
    use crate::fixtures::RECIPIENT;
    use crate::identity::ZcashIdentity;

    #[tokio::test]
    #[ignore = "Reads the public Zcash mainnet endpoints without signing or broadcasting."]
    async fn mainnet_identity_raw_transactions_history_and_quote() {
        let network =
            Network::start(Arc::new(SystemClock), Arc::new(MemoryPinStore::new()), None, &[ENTRY])
                .unwrap();
        let connector = HttpConnector::new(network.http(), Arc::new(ZcashIdentity));
        let pool = network.primary_pool(&SPEC, connector).unwrap();
        let node = Arc::new(PooledNode::new(pool).unwrap());
        let txid = "52c4eb4e838492bc2b34e1cd5d07ffc0328722934e7d4f1002d47d0fe73e54bb";
        let document = node.get(Route::Tx(txid)).await.unwrap();
        let raw = hex::decode(document["hex"].as_str().unwrap()).unwrap();
        let tx = Transaction::read(raw.as_slice(), BranchId::Sprout).unwrap();
        assert_eq!(tx.txid().to_string(), txid);
        let address = "t1ZAPbnC8oC2eF6d4cJA4qxojkR9C44eB2y";
        let adapter =
            ZcashAdapter::new(node, Arc::new(MemoryCursorStore::new()), Arc::new(SystemClock));
        adapter
            .set_watch_set(&[WatchedAddressBuilder::new(1, CHAIN, address).build()])
            .await
            .unwrap();
        let sink = FakeEventSink::new();
        let mut is_complete = false;
        for _ in 0..4 {
            let deadline = SystemClock.deadline_in(Duration::from_secs(120));
            if adapter.sync(deadline, &sink).await.unwrap().is_complete() {
                is_complete = true;
                break;
            }
            sleep(Duration::from_secs(30)).await;
        }
        assert!(is_complete);
        assert!(sink.len() > 1);
        let mut request =
            TxRequestBuilder::new(1, RECIPIENT, AssetId::Native(CHAIN), Amount::ZERO).build();
        request.amount = SendAmount::Max;
        let quote = adapter.quote_fee(&request).await.unwrap();
        assert!(quote.fee.base_units() >= 10_000);
        assert!(quote.max_amount.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn requests_wait_when_no_endpoint_attempt_was_admitted() {
        let calls = AtomicUsize::new(0);
        let value = retry_until_admitted("test read", || {
            let count = calls.fetch_add(1, Ordering::SeqCst);
            async move {
                if count < 2 {
                    Err(NetError::NoneReady { chain: CHAIN, operation: "test read" })
                } else {
                    Ok(7)
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(value, 7);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test(start_paused = true)]
    async fn an_attempted_submission_is_never_repeated() {
        let calls = AtomicUsize::new(0);
        let error = retry_until_admitted("test broadcast", || {
            calls.fetch_add(1, Ordering::SeqCst);
            async {
                Err::<(), _>(NetError::AllFailed {
                    chain: CHAIN,
                    operation: "test broadcast",
                    attempts: Vec::new(),
                })
            }
        })
        .await
        .unwrap_err();
        assert!(matches!(error, NetError::AllFailed { .. }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
