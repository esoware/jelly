use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{EndpointLabel, SecretText};
use jelly_net::{
    Endpoint, EndpointOrigin, HttpEndpoint, HttpRequest, NetError, Outcome, Pool, Standing,
};
use serde::{Deserialize, de::DeserializeOwned};
use zeroize::Zeroizing;

use crate::config::CHAIN;
use crate::error::TronError;
use crate::route::{Request, Route};

/// Budget for one HTTP exchange. The pool bounds the whole rotation on top of it.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The header `TronGrid` reads an API key from. The key raises a rate limit but
/// is never required.
const API_KEY_HEADER: &str = "TRON-PRO-API-KEY";

/// `TronGrid` is the only host that serves the `/v1` indexer routes. Its key is a
/// `TronGrid` credential, so it goes to this host over TLS and to no other node in
/// the pool, curated or added by the user.
const TRONGRID_HOST: &str = "api.trongrid.io";

fn is_trongrid(endpoint: &EndpointLabel) -> bool {
    endpoint.scheme() == "https" && endpoint.host() == TRONGRID_HOST
}

/// A full node answers 404 for the `/v1` routes, so it is never asked for them.
fn indexer_standing(endpoint: &Endpoint) -> Standing {
    if is_trongrid(endpoint.label()) { Standing::Preferred } else { Standing::Excluded }
}

/// `TronGrid` serves the `/wallet` routes only behind every full node that is not
/// failing.
///
/// History runs on `TronGrid`'s free budget, and the balance reads every pass
/// makes would spend it. A pin is the user's explicit choice and puts it back
/// among the full nodes.
fn wallet_standing(endpoint: &Endpoint) -> Standing {
    if is_trongrid(endpoint.label()) && endpoint.origin() != EndpointOrigin::Pinned {
        Standing::Fallback
    } else {
        Standing::Preferred
    }
}

/// Every failure rotates.
///
/// Only a transport failure reaches here, because [`TronNode`] returns the body
/// unparsed and every decode happens after the walk. TRON's HTTP API has no
/// transport-level refusal that every node would repeat, so a failure here is worth
/// trying elsewhere.
const fn classify(_: &NetError) -> Outcome {
    Outcome::Rotate
}

/// Reads retry through the pool, and a broadcast makes one endpoint attempt.
///
/// When `/wallet/broadcasthex` answers 404 or 405, the broadcast tries the
/// fallback route on the same endpoint. Any other transport failure leaves the
/// outcome unknown. A response that fails decoding does not trigger failover.
#[async_trait]
pub trait TronNode: Send + Sync + fmt::Debug {
    async fn call(&self, request: &Request) -> Result<Answered, TronError>;

    async fn broadcast(
        &self,
        preferred: &Request,
        fallback: &Request,
    ) -> Result<Answered, TronError>;
}

/// An unparsed body that carries its endpoint, so an unreadable shape is
/// reported against the host that produced it.
#[derive(Debug, Clone)]
pub struct Answered {
    endpoint: EndpointLabel,
    route: Route,
    body: String,
}

impl Answered {
    pub const fn new(endpoint: EndpointLabel, route: Route, body: String) -> Self {
        Self { endpoint, route, body }
    }

    /// A body carrying TRON's `{"Error": ...}` member becomes
    /// [`TronError::Fault`], because several nodes report a refusal that way with
    /// HTTP 200. An unreadable body is reported by line and column only, because
    /// serde's message embeds the input.
    pub(crate) fn decode<T: DeserializeOwned>(&self) -> Result<T, TronError> {
        if let Ok(fault) = serde_json::from_str::<Fault>(&self.body)
            && let Some(detail) = fault.message()
        {
            return Err(TronError::Fault {
                endpoint: self.endpoint.clone(),
                route: self.route,
                detail,
            });
        }
        serde_json::from_str(&self.body).map_err(|source| TronError::Answer {
            endpoint: self.endpoint.clone(),
            route: self.route,
            line: source.line(),
            column: source.column(),
        })
    }

    /// A field that parsed and cannot be used, named without quoting it.
    pub(crate) fn unusable(&self, what: &'static str) -> TronError {
        TronError::Shape { endpoint: self.endpoint.clone(), route: self.route, what }
    }
}

/// A few nodes answer a well-formed request with `{"Error": "..."}` and HTTP
/// 200. `error` in lower case appears on some gateways.
#[derive(Debug, Deserialize)]
struct Fault {
    #[serde(rename = "Error", default)]
    upper: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

impl Fault {
    fn message(self) -> Option<String> {
        self.upper.or(self.error).filter(|text| !text.trim().is_empty())
    }
}

/// TRON's HTTP API over the shared client and failover pool.
///
/// The pool's connector runs [`crate::TronIdentity`]'s genesis check once per
/// endpoint per session, so a host serving another network is disqualified before
/// it can answer a balance.
///
/// The `/v1` routes go to `TronGrid` alone and the `/wallet` routes to the full
/// nodes first, so one pool and one health record per host serve both.
pub struct PooledNode {
    pool: Arc<Pool<HttpEndpoint>>,
    api_key: Option<SecretText>,
}

impl fmt::Debug for PooledNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PooledNode")
            .field("endpoints", &self.pool.endpoints().len())
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

impl PooledNode {
    /// A pool for another chain or with no endpoints fails here as a
    /// configuration mistake.
    ///
    /// The API key lives only here, so no printable configuration value holds it.
    pub fn new(
        pool: Arc<Pool<HttpEndpoint>>,
        api_key: Option<SecretText>,
    ) -> Result<Self, TronError> {
        let chain = pool.chain();
        if chain != CHAIN {
            return Err(TronError::ForeignChain { chain });
        }
        if pool.endpoints().is_empty() {
            return Err(TronError::Net(NetError::NoEndpoints { chain }));
        }
        let api_key = api_key.filter(|key| key.with_str(|text| !text.trim().is_empty()));
        Ok(Self { pool, api_key })
    }

    async fn exchange(
        &self,
        endpoint: &HttpEndpoint,
        request: &Request,
    ) -> Result<Answered, NetError> {
        let key = self
            .api_key
            .as_ref()
            .filter(|_| is_trongrid(endpoint.label()))
            .map(|key| key.with_str(|text| Zeroizing::new(text.to_owned())));
        let key_header = key.as_deref().map(|key| (API_KEY_HEADER, key.as_str()));
        let route = request.route();
        let http = match request.body() {
            Some(body) => HttpRequest::post_json(body, REQUEST_TIMEOUT),
            None => HttpRequest::get(REQUEST_TIMEOUT),
        };
        // Always the merging form, which equals `send` for an empty query and is
        // the only one that keeps a query in the endpoint's own address.
        let body = endpoint
            .send_query(
                request.path(),
                request.query(),
                http.with_headers(key_header.as_slice()).with_max_bytes(route.max_response_bytes()),
            )
            .await?
            .into_body()?;
        Ok(Answered::new(endpoint.label().clone(), route, body))
    }
}

#[async_trait]
impl TronNode for PooledNode {
    async fn call(&self, request: &Request) -> Result<Answered, TronError> {
        let route = request.route();
        let standing: fn(&Endpoint) -> Standing =
            if route.is_indexer() { indexer_standing } else { wallet_standing };
        self.pool
            .call_ranked(route.as_str(), classify, standing, async |endpoint: Arc<HttpEndpoint>| {
                self.exchange(&endpoint, request).await
            })
            .await
            .map_err(TronError::Net)
    }

    async fn broadcast(
        &self,
        preferred: &Request,
        fallback: &Request,
    ) -> Result<Answered, TronError> {
        self.pool
            .broadcast_once_ranked(
                preferred.route().as_str(),
                classify,
                wallet_standing,
                async |endpoint: Arc<HttpEndpoint>| {
                    match self.exchange(&endpoint, preferred).await {
                        Ok(answer) => Ok(answer),
                        // A 404 or 405 means the gateway does not serve that
                        // route, so nothing was accepted. Anything else leaves
                        // the outcome unknown and must not be retried.
                        Err(NetError::Status { status: 404 | 405, .. }) => {
                            self.exchange(&endpoint, fallback).await
                        }
                        Err(other) => Err(other),
                    }
                },
            )
            .await
            .map_err(TronError::Net)
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use jelly_core::{ChainId, Clock, FailureKind, SystemClock, Transport};
    use jelly_net::{Connect, Identity, NetRuntime};

    use super::*;
    use crate::fixtures::answered;
    use crate::shape::NowBlock;
    use crate::test_util::endpoint;

    /// These checks are about construction, so the connector never dials.
    #[derive(Debug)]
    struct NeverDials;

    #[async_trait]
    impl Connect<HttpEndpoint> for NeverDials {
        async fn connect(&self, _endpoint: &Endpoint) -> Result<Arc<HttpEndpoint>, NetError> {
            Err(NetError::NoEndpoints { chain: ChainId::Tron })
        }

        async fn identify(
            &self,
            _endpoint: &Endpoint,
            _connection: &HttpEndpoint,
        ) -> Result<Identity, NetError> {
            Ok(Identity::Unchecked)
        }
    }

    fn pool(chain: ChainId, urls: &[&str]) -> Arc<Pool<HttpEndpoint>> {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let runtime = NetRuntime::new(clock, &[]);
        let endpoints = urls
            .iter()
            .map(|url| Endpoint::parse(chain, Transport::TronHttp, url).expect("a canonical url"))
            .collect();
        Arc::new(runtime.pool(chain, Transport::TronHttp, Arc::new(NeverDials), endpoints))
    }

    #[test]
    fn every_failure_the_walk_can_see_moves_it_on() {
        let failures = [
            NetError::NoEndpoints { chain: ChainId::Tron },
            NetError::Status { endpoint: endpoint(), status: 404 },
            NetError::endpoint(&endpoint(), FailureKind::Connect, io::Error::other("refused")),
        ];
        for failure in &failures {
            assert_eq!(classify(failure), Outcome::Rotate, "{failure:?}");
        }
    }

    #[test]
    fn a_pool_for_another_chain_or_with_no_endpoints_is_refused_at_construction() {
        let wrong = PooledNode::new(pool(ChainId::Ethereum, &["https://node.invalid"]), None);
        assert!(matches!(wrong, Err(TronError::ForeignChain { chain: ChainId::Ethereum })));

        let empty = PooledNode::new(pool(ChainId::Tron, &[]), None);
        assert!(matches!(empty, Err(TronError::Net(NetError::NoEndpoints { .. }))));
    }

    #[test]
    fn a_blank_api_key_is_the_same_as_none_and_never_reaches_a_debug_line() {
        let node = PooledNode::new(
            pool(ChainId::Tron, &["https://node.invalid"]),
            Some(SecretText::new(String::from("   "))),
        )
        .expect("it builds");
        assert!(node.api_key.is_none());

        let keyed = PooledNode::new(
            pool(ChainId::Tron, &["https://node.invalid"]),
            Some(SecretText::new(String::from("SECRET-KEY"))),
        )
        .expect("it builds");
        let rendered = format!("{keyed:?}");
        assert!(!rendered.contains("SECRET-KEY"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[test]
    fn the_api_key_goes_to_trongrid_over_tls_and_nowhere_else() {
        assert!(is_trongrid(&EndpointLabel::new("https", "api.trongrid.io", None)));
        for (scheme, host) in [
            ("http", "api.trongrid.io"),
            ("https", "tron-rpc.publicnode.com"),
            ("https", "api.trongrid.io.example"),
            ("https", "nile.trongrid.io"),
            ("http", "127.0.0.1"),
        ] {
            let endpoint = EndpointLabel::new(scheme, host, None);
            assert!(!is_trongrid(&endpoint), "{endpoint}");
        }
    }

    #[test]
    fn a_node_that_reports_a_refusal_as_http_200_is_not_read_as_an_answer() {
        let fault: Result<serde_json::Value, TronError> =
            answered(Route::Account, r#"{"Error":"class org.tron.core Exception"}"#).decode();
        let error = fault.expect_err("a fault is not an answer");
        assert!(matches!(error, TronError::Fault { route: Route::Account, .. }), "{error:?}");

        let lower: Result<serde_json::Value, TronError> =
            answered(Route::Account, r#"{"error":"bad request"}"#).decode();
        assert!(matches!(lower, Err(TronError::Fault { .. })));

        // An empty Error member is what a healthy node sends for an absent row.
        let fine: serde_json::Value =
            answered(Route::Account, r#"{"Error":""}"#).decode().expect("not a fault");
        assert!(fine.is_object());
    }

    #[test]
    fn an_unreadable_answer_is_reported_by_position_and_never_quoted() {
        let error = answered(Route::NowBlock, r#"{"blockID":7}"#)
            .decode::<NowBlock>()
            .expect_err("a number is not a block id");
        let rendered = error.to_string();
        assert!(rendered.contains("line 1"), "{rendered}");
        assert!(rendered.contains("getblock"), "{rendered}");
        assert!(!rendered.contains("blockID"), "{rendered}");
    }
}
