use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{EndpointLabel, FailureKind};
use jelly_net::{HttpEndpoint, HttpRequest, JsonRpcBatch, JsonRpcRequest, NetError, Pool};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::{RawValue, to_raw_value};

use crate::broadcast::BroadcastReply;
use crate::config::CHAIN;
use crate::error::{self, INVALID_REQUEST, PARSE_ERROR, SolanaError};
use crate::method::Method;

const TARGET: &str = "jelly::solana::node";

/// Budget for one request against one endpoint. The pool bounds the whole
/// rotation separately.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// A method and its parameter list, serialised once at construction so a
/// rotation posts identical bytes to every endpoint.
#[derive(Debug)]
pub struct RpcCall {
    method: Method,
    params: Box<RawValue>,
}

impl RpcCall {
    /// `params` is the whole positional list, usually a tuple.
    pub(crate) fn new<P: Serialize>(method: Method, params: &P) -> Result<Self, SolanaError> {
        let params =
            to_raw_value(params).map_err(|_| SolanaError::Encode { what: "rpc parameters" })?;
        Ok(Self { method, params })
    }

    pub const fn method(&self) -> Method {
        self.method
    }

    pub fn params(&self) -> &RawValue {
        &self.params
    }
}

/// Unparsed results that carry their endpoint, so an unreadable shape is
/// reported against the host that produced it.
#[derive(Debug, Clone)]
pub struct Answered {
    endpoint: EndpointLabel,
    results: Vec<Box<RawValue>>,
}

impl Answered {
    pub const fn new(endpoint: EndpointLabel, results: Vec<Box<RawValue>>) -> Self {
        Self { endpoint, results }
    }

    pub(crate) const fn endpoint(&self) -> &EndpointLabel {
        &self.endpoint
    }

    pub(crate) fn decode<T: DeserializeOwned>(&self, method: Method) -> Result<T, SolanaError> {
        self.decode_at(0, method)
    }

    pub(crate) fn decode_at<T: DeserializeOwned>(
        &self,
        index: usize,
        method: Method,
    ) -> Result<T, SolanaError> {
        let raw = self.results.get(index).ok_or(SolanaError::BatchLength {
            endpoint: self.endpoint.clone(),
            method,
            asked: index.saturating_add(1),
            answered: self.results.len(),
        })?;
        serde_json::from_str(raw.get())
            .map_err(|source| SolanaError::unreadable(&self.endpoint, method, &source))
    }

    /// Answers are read by position, so a set of the wrong length is refused
    /// whole.
    pub(crate) fn check_count(&self, method: Method, asked: usize) -> Result<(), SolanaError> {
        if self.results.len() == asked {
            return Ok(());
        }
        Err(SolanaError::BatchLength {
            endpoint: self.endpoint.clone(),
            method,
            asked,
            answered: self.results.len(),
        })
    }
}

/// Node operations with distinct retry rules.
///
/// - `call` tries endpoints until one answers.
/// - `batch` keeps call order and uses as few round trips as the endpoint
///   allows.
/// - `broadcast` tries one endpoint only, so a transport failure leaves the
///   outcome unknown.
///
/// Answers from `call` and `batch` are unparsed. A response that fails decoding
/// is reported against its endpoint rather than retried through the pool.
#[async_trait]
pub trait SolanaNode: Send + Sync + fmt::Debug {
    async fn call(&self, call: &RpcCall) -> Result<Answered, SolanaError>;

    async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, SolanaError>;

    async fn broadcast(&self, call: &RpcCall) -> Result<BroadcastReply, SolanaError>;
}

/// JSON-RPC over the shared HTTP client and failover pool.
///
/// The pool's connector runs [`crate::SolanaIdentity`]'s `getGenesisHash` check
/// once per endpoint per session, so a host serving a test cluster is
/// disqualified before it can answer a balance.
pub struct PooledNode {
    pool: Arc<Pool<HttpEndpoint>>,
    next_id: AtomicU64,
    /// Cleared for the session once an endpoint refuses or silently drops batch
    /// documents. After that every batch is sent one call at a time.
    is_batching: AtomicBool,
}

impl fmt::Debug for PooledNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PooledNode")
            .field("chain", &self.pool.chain())
            .field("endpoints", &self.pool.endpoints().len())
            .field("is_batching", &self.is_batching.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl PooledNode {
    /// A pool for another chain or with no endpoints fails here as a
    /// configuration mistake.
    pub fn new(pool: Arc<Pool<HttpEndpoint>>) -> Result<Self, SolanaError> {
        let chain = pool.chain();
        if chain != CHAIN {
            return Err(SolanaError::ForeignChain { chain });
        }
        if pool.endpoints().is_empty() {
            return Err(SolanaError::Net(NetError::NoEndpoints { chain }));
        }
        Ok(Self { pool, next_id: AtomicU64::new(1), is_batching: AtomicBool::new(true) })
    }

    /// True while this deployment's endpoints still accept batch documents.
    #[cfg(feature = "test-util")]
    pub fn is_batching(&self) -> bool {
        self.is_batching.load(Ordering::SeqCst)
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn document(&self, call: &RpcCall) -> Result<JsonRpcRequest, SolanaError> {
        JsonRpcRequest::new(self.next_id(), call.method().as_str(), &call.params())
            .map_err(SolanaError::Net)
    }

    async fn one(&self, call: &RpcCall) -> Result<Answered, SolanaError> {
        let method = call.method();
        let request = self.document(call)?;
        self.pool
            .call(method.as_str(), error::classify, move |endpoint: Arc<HttpEndpoint>| {
                let request = request.clone();
                async move {
                    let result = endpoint.rpc::<Box<RawValue>>(&request, REQUEST_TIMEOUT).await?;
                    Ok(Answered::new(endpoint.label().clone(), vec![result]))
                }
            })
            .await
            .map_err(|error| SolanaError::from_net(method, error))
    }

    /// Every call, one at a time, for an endpoint that will not take a document.
    async fn sequentially(
        &self,
        first: &RpcCall,
        rest: &[RpcCall],
    ) -> Result<Answered, SolanaError> {
        let mut answered = self.one(first).await?;
        for call in rest {
            answered.results.extend(self.one(call).await?.results);
        }
        Ok(answered)
    }
}

#[async_trait]
impl SolanaNode for PooledNode {
    async fn call(&self, call: &RpcCall) -> Result<Answered, SolanaError> {
        self.one(call).await
    }

    async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, SolanaError> {
        let Some((first, rest)) = calls.split_first() else {
            return Err(SolanaError::EmptyBatch);
        };
        if rest.is_empty() || !self.is_batching.load(Ordering::SeqCst) {
            return self.sequentially(first, rest).await;
        }

        // `jelly_net` matches answers to these ids, reorders them and checks the
        // length before any is read.
        let items = calls.iter().map(|call| self.document(call)).collect::<Result<Vec<_>, _>>()?;
        let batch = JsonRpcBatch::new(&items);
        // A document mixes methods, so it is walked as an ordinary read labelled
        // with the first call's method.
        let method = first.method();
        let walked = self
            .pool
            .call(method.as_str(), error::classify, move |endpoint: Arc<HttpEndpoint>| {
                let batch = batch.clone();
                async move {
                    let results = endpoint.rpc_batch(&batch, REQUEST_TIMEOUT).await?;
                    Ok(Answered::new(endpoint.label().clone(), results))
                }
            })
            .await
            .map_err(|error| SolanaError::from_net(method, error));

        match walked {
            Ok(answered) => Ok(answered),
            Err(error) if should_retry_singly(&error) => {
                if is_document_refusal(&error) {
                    tracing::info!(
                        target: TARGET,
                        %error,
                        "an endpoint refused a batch document; this session sends one call at a time"
                    );
                    self.is_batching.store(false, Ordering::SeqCst);
                }
                let singly = self.sequentially(first, rest).await;
                // Singles answering where the document did not is the only
                // evidence of a host that hangs on batch documents instead of
                // refusing them. Without this, every operation would burn a full
                // timeout on that host for the whole session.
                if singly.is_ok() && self.is_batching.load(Ordering::SeqCst) {
                    tracing::info!(
                        target: TARGET,
                        %error,
                        "batch documents fail here but single calls answer; \
                         this session sends one call at a time"
                    );
                    self.is_batching.store(false, Ordering::SeqCst);
                }
                singly
            }
            Err(error) => Err(error),
        }
    }

    async fn broadcast(&self, call: &RpcCall) -> Result<BroadcastReply, SolanaError> {
        let request = self.document(call)?;
        self.pool
            .broadcast_once(
                Method::SendTransaction.as_str(),
                error::classify,
                move |endpoint: Arc<HttpEndpoint>| {
                    let request = request.clone();
                    async move {
                        let label = endpoint.label();
                        let body = endpoint
                            .send("", HttpRequest::post_json(request.as_str(), REQUEST_TIMEOUT))
                            .await?
                            .into_body()?;
                        Ok(BroadcastReply::read(label, &body)?)
                    }
                },
            )
            .await
            .map_err(|error| SolanaError::from_net(Method::SendTransaction, error))
    }
}

/// True when the batch document itself was refused, by a non-batch answer, a
/// wrong answer count, an oversized payload or an unparseable body.
///
/// A per-call error such as `-32601` or `-32602` does not count, because the
/// same call sent alone would fail too.
const fn is_document_refusal(error: &SolanaError) -> bool {
    matches!(
        *error,
        SolanaError::BatchLength { .. }
            | SolanaError::Rpc { code: PARSE_ERROR | INVALID_REQUEST, .. }
            | SolanaError::Net(NetError::Protocol { .. } | NetError::Status { status: 413, .. })
    )
}

/// Batch refusals and timeouts earn one retry as single calls, because some
/// gateways ignore batches but answer single calls. Connection failures do not,
/// because singles would fail too.
fn should_retry_singly(error: &SolanaError) -> bool {
    if is_document_refusal(error) {
        return true;
    }
    let SolanaError::Net(ref net) = *error else {
        return false;
    };
    match *net {
        NetError::Timeout { .. } => true,
        NetError::Endpoint { kind, .. } => kind == FailureKind::Timeout,
        NetError::AllFailed { ref attempts, .. } => attempts
            .iter()
            .any(|attempt| matches!(attempt.kind, FailureKind::Timeout | FailureKind::Protocol)),
        NetError::Protocol { .. }
        | NetError::TooLarge { .. }
        | NetError::Status { .. }
        | NetError::RateLimited { .. }
        | NetError::Rpc { .. }
        | NetError::WrongNetwork { .. }
        | NetError::NoEndpoints { .. }
        | NetError::NoneReady { .. }
        | NetError::InvalidUrl { .. }
        | NetError::Defaults { .. }
        | NetError::Config { .. } => false,
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::{ChainId, EndpointAttempt};

    use super::*;
    use crate::rpc::{Commitment, CommitmentQuery};
    use crate::test_util::endpoint;

    fn answered(results: &[&str]) -> Answered {
        Answered::new(
            endpoint(),
            results
                .iter()
                .map(|text| RawValue::from_string((*text).to_owned()).expect("valid json"))
                .collect(),
        )
    }

    fn slot_call() -> RpcCall {
        RpcCall::new(Method::GetSlot, &(CommitmentQuery { commitment: Commitment::Confirmed },))
            .expect("it serialises")
    }

    #[test]
    fn a_call_serialises_its_parameter_list_and_keeps_its_method() {
        let call = RpcCall::new(
            Method::GetBalance,
            &(
                "9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",
                CommitmentQuery { commitment: Commitment::Confirmed },
            ),
        )
        .expect("it serialises");
        assert_eq!(call.method(), Method::GetBalance);
        assert_eq!(
            call.params().get(),
            r#"["9WzDXwBbmkg8ZTbNMqUxvQRAyrZzDsGYdLVL9zYtAWWM",{"commitment":"confirmed"}]"#
        );
        assert_eq!(slot_call().params().get(), r#"[{"commitment":"confirmed"}]"#);
    }

    #[test]
    fn an_answer_decodes_by_position_and_names_the_endpoint_when_it_cannot() {
        let batch = answered(&["7", "null", "{\"nope\":1}"]);
        assert_eq!(batch.decode_at::<u64>(0, Method::GetSlot).expect("a slot"), 7);
        assert_eq!(batch.decode_at::<Option<u64>>(1, Method::GetSlot).expect("null"), None);

        let error =
            batch.decode_at::<u64>(2, Method::GetSlot).expect_err("an object is not a slot number");
        let rendered = error.to_string();
        assert!(rendered.contains("node.invalid"), "{rendered}");
        assert!(rendered.contains("getSlot"), "{rendered}");
        assert!(rendered.contains("line 1"), "{rendered}");
        assert!(!rendered.contains("nope"), "{rendered}");

        let missing = batch.decode_at::<u64>(9, Method::GetSlot).expect_err("out of range");
        assert!(matches!(missing, SolanaError::BatchLength { .. }), "{missing:?}");
        batch.check_count(Method::GetSlot, 3).expect("three calls, three answers");
        assert!(matches!(
            batch.check_count(Method::GetSlot, 4),
            Err(SolanaError::BatchLength { asked: 4, answered: 3, .. })
        ));
    }

    fn rpc(code: i64) -> SolanaError {
        SolanaError::Rpc {
            endpoint: endpoint(),
            method: Method::GetSlot,
            code,
            message: String::new(),
        }
    }

    #[test]
    fn only_a_failure_of_the_document_itself_counts_as_a_document_refusal() {
        for code in [-32700, -32600] {
            assert!(is_document_refusal(&rpc(code)), "{code}");
        }
        // An answer that is not a batch at all is how a host that does not take
        // documents usually says so.
        assert!(is_document_refusal(&SolanaError::Net(NetError::protocol(
            &endpoint(),
            "invalid type: map, expected a sequence"
        ))));
        assert!(is_document_refusal(&SolanaError::Net(NetError::Status {
            endpoint: endpoint(),
            status: 413
        })));
        assert!(is_document_refusal(&SolanaError::BatchLength {
            endpoint: endpoint(),
            method: Method::GetSlot,
            asked: 3,
            answered: 2,
        }));

        // One call inside a document naming a method or arguments this host will
        // not serve says nothing about documents.
        for code in [-32601, -32602, -32005] {
            assert!(!is_document_refusal(&rpc(code)), "{code}");
        }
        assert!(!is_document_refusal(&SolanaError::Net(NetError::Status {
            endpoint: endpoint(),
            status: 429
        })));
        assert!(!is_document_refusal(&SolanaError::AmountZero));
    }

    #[test]
    fn a_rate_limit_does_not_retry_the_batch_as_single_calls() {
        assert!(!should_retry_singly(&SolanaError::Net(NetError::RateLimited {
            endpoint: endpoint(),
            retry_after: None,
        })));
        assert!(!should_retry_singly(&SolanaError::Net(NetError::AllFailed {
            chain: ChainId::Solana,
            operation: "getSignaturesForAddress",
            attempts: vec![EndpointAttempt::new(
                endpoint(),
                FailureKind::RateLimited,
                String::from("rate limited"),
            )],
        })));
    }

    /// A host that silently drops batch documents looks like a timeout, so a
    /// timeout earns one retry as single calls. A failure single calls would hit
    /// too does not.
    #[test]
    fn a_timeout_shaped_failure_earns_a_single_call_retry() {
        assert!(should_retry_singly(&SolanaError::Net(NetError::Timeout {
            endpoint: endpoint(),
            elapsed_millis: 15_000,
        })));
        assert!(should_retry_singly(&SolanaError::Net(NetError::AllFailed {
            chain: ChainId::Solana,
            operation: "getBalance",
            attempts: vec![EndpointAttempt::new(
                endpoint(),
                FailureKind::Timeout,
                String::from("timed out"),
            )],
        })));
        assert!(should_retry_singly(&rpc(-32600)), "a stated refusal is retried too");

        assert!(!should_retry_singly(&SolanaError::Net(NetError::AllFailed {
            chain: ChainId::Solana,
            operation: "getBalance",
            attempts: vec![EndpointAttempt::new(
                endpoint(),
                FailureKind::Connect,
                String::from("connection refused"),
            )],
        })));
        assert!(!should_retry_singly(&rpc(-32601)), "a per-call complaint is not about documents");
    }
}
