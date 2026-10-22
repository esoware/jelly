use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::EndpointLabel;
use jelly_net::{HttpEndpoint, JsonRpcBatch, JsonRpcRequest, NetError, Outcome, Pool};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::value::{RawValue, to_raw_value};

use crate::error::{self, EvmError};
use crate::method::Method;
use crate::params::EvmParams;

/// A method and its parameter list, serialised once at construction so a
/// rotation posts identical bytes to every endpoint.
#[derive(Debug)]
pub struct RpcCall {
    method: Method,
    params: Box<RawValue>,
}

impl RpcCall {
    /// `params` is the whole positional list, usually a tuple.
    pub(crate) fn new<P: Serialize>(method: Method, params: &P) -> Result<Self, EvmError> {
        let params = to_raw_value(params)
            .map_err(|source| EvmError::Encode { what: method.as_str(), source })?;
        Ok(Self { method, params })
    }

    pub(crate) fn bare(method: Method) -> Result<Self, EvmError> {
        Self::new(method, &[(); 0])
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

    /// Answers are read by position, so a batch of the wrong length is refused
    /// whole.
    pub(crate) fn check_count(&self, method: Method, asked: usize) -> Result<(), EvmError> {
        if self.results.len() == asked {
            return Ok(());
        }
        Err(EvmError::answer(
            &self.endpoint,
            method,
            format!("a batch of {asked} answered with {}", self.results.len()),
        ))
    }

    pub(crate) fn decode<T: DeserializeOwned>(&self, method: Method) -> Result<T, EvmError> {
        self.decode_at(0, method)
    }

    pub(crate) fn decode_at<T: DeserializeOwned>(
        &self,
        index: usize,
        method: Method,
    ) -> Result<T, EvmError> {
        let raw = self.results.get(index).ok_or_else(|| {
            EvmError::answer(
                &self.endpoint,
                method,
                format!("there is no answer at position {index}"),
            )
        })?;
        // A parse failure is reported by position only, because serde's message
        // quotes the input back.
        serde_json::from_str(raw.get()).map_err(|source| {
            EvmError::answer(
                &self.endpoint,
                method,
                format!("unreadable at line {} column {}", source.line(), source.column()),
            )
        })
    }
}

/// Node operations with distinct retry rules.
///
/// - `call` tries endpoints until one answers.
/// - `batch` sends one document, and results keep request order.
/// - `broadcast` tries one endpoint only, so a transport failure leaves the
///   outcome unknown.
///
/// Results are unparsed. A response that fails decoding is reported against its
/// endpoint rather than retried through the pool.
#[async_trait]
pub trait EvmNode: Send + Sync + fmt::Debug {
    async fn call(&self, call: &RpcCall) -> Result<Answered, EvmError>;

    async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, EvmError>;

    async fn broadcast(&self, call: &RpcCall) -> Result<Answered, EvmError>;
}

/// JSON-RPC over the shared HTTP client and failover pool.
///
/// The pool's connector runs [`EvmParams`]'s `eth_chainId` check once per
/// endpoint per session, so a host serving another network is disqualified
/// before it can answer a balance.
pub struct PooledNode {
    pool: Arc<Pool<HttpEndpoint>>,
    timeout: Duration,
}

impl fmt::Debug for PooledNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PooledNode")
            .field("chain", &self.pool.chain())
            .field("endpoints", &self.pool.endpoints().len())
            .finish_non_exhaustive()
    }
}

impl PooledNode {
    /// A pool for another chain or with no endpoints fails here as a
    /// configuration mistake.
    pub fn new(params: &EvmParams, pool: Arc<Pool<HttpEndpoint>>) -> Result<Self, EvmError> {
        let chain = pool.chain();
        if chain != params.chain {
            return Err(EvmError::ForeignChain { chain });
        }
        if pool.endpoints().is_empty() {
            return Err(EvmError::Net(NetError::NoEndpoints { chain }));
        }
        Ok(Self { pool, timeout: params.request_timeout })
    }

    const fn classifier(method: Method) -> fn(&NetError) -> Outcome {
        match method {
            Method::GetLogs => error::classify_logs,
            Method::SendRawTransaction => error::classify_broadcast,
            Method::ChainId
            | Method::BlockNumber
            | Method::GetBalance
            | Method::GetTransactionCount
            | Method::Call
            | Method::EstimateGas
            | Method::FeeHistory
            | Method::GetTransactionReceipt
            | Method::GetTransactionByHash => error::classify,
        }
    }

    async fn one(&self, call: &RpcCall, attempts: Attempts) -> Result<Answered, EvmError> {
        let method = call.method();
        let request = document(1, call)?;
        let timeout = self.timeout;
        let classify = Self::classifier(method);
        let run = move |endpoint: Arc<HttpEndpoint>| {
            let request = request.clone();
            async move {
                let result = endpoint.rpc::<Box<RawValue>>(&request, timeout).await?;
                Ok(Answered::new(endpoint.label().clone(), vec![result]))
            }
        };
        let answered = match attempts {
            Attempts::UntilAnswered => self.pool.call(method.as_str(), classify, run).await,
            Attempts::Once => self.pool.broadcast_once(method.as_str(), classify, run).await,
        };
        answered.map_err(|error| EvmError::from_net(method, error))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attempts {
    UntilAnswered,
    Once,
}

#[async_trait]
impl EvmNode for PooledNode {
    async fn call(&self, call: &RpcCall) -> Result<Answered, EvmError> {
        self.one(call, Attempts::UntilAnswered).await
    }

    async fn batch(&self, calls: &[RpcCall]) -> Result<Answered, EvmError> {
        let Some(first) = calls.first() else {
            return Err(EvmError::EmptyBatch);
        };
        // Ids follow position, so each answer pairs with its own request.
        // `jelly_net` reorders the answers and checks the length.
        let mut items = Vec::with_capacity(calls.len());
        for (id, call) in (1u64..).zip(calls) {
            items.push(document(id, call)?);
        }
        let batch = JsonRpcBatch::new(&items);
        let timeout = self.timeout;
        // A batch mixes methods and never holds a broadcast, so it is walked as
        // an ordinary read.
        let method = first.method();
        self.pool
            .call(method.as_str(), error::classify, move |endpoint: Arc<HttpEndpoint>| {
                let batch = batch.clone();
                async move {
                    let results = endpoint.rpc_batch(&batch, timeout).await?;
                    Ok(Answered::new(endpoint.label().clone(), results))
                }
            })
            .await
            .map_err(|error| EvmError::from_net(method, error))
    }

    async fn broadcast(&self, call: &RpcCall) -> Result<Answered, EvmError> {
        self.one(call, Attempts::Once).await
    }
}

fn document(id: u64, call: &RpcCall) -> Result<JsonRpcRequest, EvmError> {
    JsonRpcRequest::new(id, call.method().as_str(), &call.params()).map_err(EvmError::Net)
}

#[cfg(test)]
mod tests {
    use alloy::primitives::U256;

    use super::*;
    use crate::rpc::BlockTag;
    use crate::test_util::endpoint;

    #[test]
    fn a_call_serialises_its_parameter_list_once_and_keeps_its_method() {
        let call = RpcCall::new(Method::GetTransactionCount, &("0xabc", BlockTag::Pending))
            .expect("it serialises");
        assert_eq!(call.method(), Method::GetTransactionCount);
        assert_eq!(call.params().get(), r#"["0xabc","pending"]"#);

        let bare = RpcCall::bare(Method::BlockNumber).expect("it serialises");
        assert_eq!(bare.params().get(), "[]");
    }

    fn answered(results: &[&str]) -> Answered {
        Answered::new(
            endpoint(),
            results
                .iter()
                .map(|text| RawValue::from_string((*text).to_owned()).expect("valid json"))
                .collect(),
        )
    }

    #[test]
    fn an_answer_decodes_by_position_and_names_the_endpoint_when_it_cannot() {
        let batch = answered(&["\"0x1\"", "null", "{\"nope\":1}"]);
        batch.check_count(Method::GetBalance, 3).unwrap();
        let short = batch.check_count(Method::GetBalance, 4).expect_err("one answer short");
        assert!(short.to_string().contains("a batch of 4 answered with 3"), "{short}");
        assert_eq!(
            batch.decode_at::<U256>(0, Method::GetBalance).expect("a quantity"),
            U256::from(1)
        );
        assert_eq!(batch.decode_at::<Option<u64>>(1, Method::BlockNumber).expect("null"), None);

        let error = batch
            .decode_at::<U256>(2, Method::GetBalance)
            .expect_err("an object is not a quantity");
        let rendered = error.to_string();
        assert!(rendered.contains("node.invalid"), "{rendered}");
        assert!(rendered.contains("eth_getBalance"), "{rendered}");
        assert!(rendered.contains("line 1"), "{rendered}");

        let missing = batch.decode_at::<u64>(9, Method::GetBalance).expect_err("out of range");
        assert!(missing.to_string().contains("position 9"), "{missing}");
    }

    #[test]
    fn an_empty_answer_decodes_nothing() {
        let empty = answered(&[]);
        empty.check_count(Method::BlockNumber, 0).unwrap();
        empty.decode::<u64>(Method::BlockNumber).unwrap_err();
    }
}
