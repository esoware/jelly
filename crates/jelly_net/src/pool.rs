use std::collections::HashMap;
use std::error::Error as _;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{ChainId, Clock, Deadline, EndpointAttempt, Transport, duration_millis};
use parking_lot::Mutex;

use crate::connect::{Connect, Identity};
use crate::endpoint::Endpoint;
use crate::error::{NetError, within};
use crate::health::{HealthClass, HealthTracker};
use crate::http::CONNECT_TIMEOUT;
use crate::pacer::Pacer;
use crate::runtime::NetRuntime;

const TARGET: &str = "jelly::net::pool";

/// What the failover walk does with a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The endpoint answered and this error is the answer. The walk stops and
    /// the endpoint is credited with a success.
    Accept,
    /// The endpoint failed, so it is penalized and the walk tries the next one.
    Rotate,
    /// The endpoint is healthy but does not serve this request, such as a host that
    /// refuses log queries. The walk tries the next one and the endpoint's health
    /// is untouched, because it still serves everything else.
    Skip,
    /// The request is wrong and every endpoint will say so. The walk stops and
    /// the endpoint's health is untouched.
    Fail,
}

/// Where one request puts an endpoint, for a chain whose endpoints do not all serve
/// every request alike. Declaration order is preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Standing {
    Preferred,
    /// Tried after every preferred endpoint that is as healthy.
    Fallback,
    /// Never asked, because it does not serve the request.
    Excluded,
}

/// What a chain whose endpoints all serve every request alike walks with.
const fn everywhere(_endpoint: &Endpoint) -> Standing {
    Standing::Preferred
}

/// Request budgets include dialing, TLS, identity checks, and calls.
///
/// `walk` limits the whole endpoint rotation and `per_attempt` limits one endpoint. The
/// walk stops once less than `floor` remains rather than start an attempt without
/// enough time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutPolicy {
    pub connect: Duration,
    pub per_attempt: Duration,
    pub walk: Duration,
    pub floor: Duration,
}

impl TimeoutPolicy {
    /// What every transport but Monero gets.
    const DEFAULT: Self = Self {
        connect: CONNECT_TIMEOUT,
        per_attempt: Duration::from_secs(15),
        walk: Duration::from_secs(45),
        floor: Duration::from_millis(250),
    };

    /// For Monero, whose daemon scan and distribution calls are slow by nature.
    const SLOW: Self = Self {
        connect: CONNECT_TIMEOUT,
        per_attempt: Duration::from_secs(30),
        walk: Duration::from_secs(90),
        floor: Duration::from_millis(250),
    };

    pub const fn for_transport(transport: Transport) -> Self {
        match transport {
            Transport::MoneroRpc => Self::SLOW,
            Transport::ElectrumSsl
            | Transport::HttpJsonrpc
            | Transport::WsJsonrpc
            | Transport::Blockbook
            | Transport::TronHttp
            | Transport::FeeApi
            | Transport::HistoryApi => Self::DEFAULT,
        }
    }
}

/// Whether a walk may try another endpoint after one it tried failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Rotation {
    Failover,
    /// One endpoint at most, because a failed send leaves its outcome unknown.
    Once,
}

/// What the pool knows about an endpoint whose network it has asked about.
///
/// A disqualified endpoint has no connection to hold, so none can be cached for it.
enum Admission<C> {
    /// The endpoint proved its network, with the connection that is open to it, if any.
    Verified { connection: Option<Arc<C>> },
    /// What the check that disqualified the endpoint expected and found, so a walk that
    /// learns of it late can fail with the same answer.
    Disqualified { expected: String, found: String },
}

impl<C> Admission<C> {
    /// The failure a disqualified endpoint answers every later request with.
    fn refusal(&self, endpoint: &Endpoint) -> Option<NetError> {
        match self {
            Self::Verified { .. } => None,
            Self::Disqualified { expected, found } => Some(NetError::WrongNetwork {
                endpoint: endpoint.label().clone(),
                expected: expected.clone(),
                found: found.clone(),
            }),
        }
    }
}

/// A failure from one attempt, and whether the caller's classifier gets a say.
enum Failed {
    /// The walk rotates whatever the classifier would say. Only an identity check that
    /// failed or found another network does this, because the classifier judges the
    /// caller's request, and no caller may accept an answer from a host that has not
    /// proved its network.
    Rotate(NetError),
    Classify(NetError),
}

/// This is the workspace's only failover walk.
///
/// Rotation order, per-attempt timing, failure aggregation, health recording, and
/// admission control all live here. A chain crate supplies a connector, a closure, and
/// a classifier, and gets one answer or one aggregate failure.
pub struct Pool<C> {
    chain: ChainId,
    transport: Transport,
    timeouts: TimeoutPolicy,
    endpoints: Vec<Endpoint>,
    connector: Arc<dyn Connect<C>>,
    health: Arc<HealthTracker>,
    pacer: Arc<Pacer>,
    clock: Arc<dyn Clock>,
    admissions: Mutex<HashMap<String, Admission<C>>>,
}

impl<C> fmt::Debug for Pool<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Pool")
            .field("chain", &self.chain)
            .field("transport", &self.transport)
            .field("timeouts", &self.timeouts)
            .field("endpoints", &self.endpoints.len())
            .finish_non_exhaustive()
    }
}

impl<C: Send + Sync + 'static> Pool<C> {
    /// A pool over `runtime`'s clock, health tracker and pacer, with the
    /// transport's own timeout policy.
    ///
    /// Only [`NetRuntime::pool`] builds one, because a pool that did not share the
    /// runtime's bookkeeping would rotate on one notion of time and record on another.
    pub(crate) fn new(
        chain: ChainId,
        transport: Transport,
        connector: Arc<dyn Connect<C>>,
        endpoints: Vec<Endpoint>,
        runtime: &NetRuntime,
    ) -> Self {
        Self {
            chain,
            transport,
            timeouts: TimeoutPolicy::for_transport(transport),
            endpoints,
            connector,
            health: Arc::clone(runtime.health()),
            pacer: Arc::clone(runtime.pacer()),
            clock: Arc::clone(runtime.clock()),
            admissions: Mutex::new(HashMap::new()),
        }
    }

    pub const fn chain(&self) -> ChainId {
        self.chain
    }

    pub fn endpoints(&self) -> &[Endpoint] {
        &self.endpoints
    }

    /// Walks the endpoints in health order until one answers, `classify` stops
    /// the walk, or the walk deadline runs out.
    pub async fn call<T, F, Fut>(
        &self,
        operation: &'static str,
        classify: fn(&NetError) -> Outcome,
        f: F,
    ) -> Result<T, NetError>
    where
        T: Send,
        F: Fn(Arc<C>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, NetError>> + Send,
    {
        self.walk(operation, classify, everywhere, Rotation::Failover, f).await
    }

    /// [`Pool::call`] over the endpoints `standing` admits, in the order it ranks
    /// them. An endpoint it excludes is never connected for this request.
    pub async fn call_ranked<T, F, Fut>(
        &self,
        operation: &'static str,
        classify: fn(&NetError) -> Outcome,
        standing: fn(&Endpoint) -> Standing,
        f: F,
    ) -> Result<T, NetError>
    where
        T: Send,
        F: Fn(Arc<C>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, NetError>> + Send,
    {
        self.walk(operation, classify, standing, Rotation::Failover, f).await
    }

    /// It makes one attempt against one endpoint.
    ///
    /// A transport failure leaves the outcome unknown, so a broadcast is never replayed
    /// around a rotation. The caller re-checks by transaction id and re-sends the
    /// identical bytes, or gives up.
    pub async fn broadcast_once<T, F, Fut>(
        &self,
        operation: &'static str,
        classify: fn(&NetError) -> Outcome,
        f: F,
    ) -> Result<T, NetError>
    where
        T: Send,
        F: Fn(Arc<C>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, NetError>> + Send,
    {
        self.walk(operation, classify, everywhere, Rotation::Once, f).await
    }

    /// One attempt against the configured endpoint whose URL is `url`, connecting to it
    /// first when no connection is open, for a request that must reach that node and no
    /// other. An endpoint the user removed fails as [`NetError::NoEndpoints`], and one
    /// found serving another network as [`NetError::WrongNetwork`].
    pub async fn call_at<T, F, Fut>(
        &self,
        url: &str,
        operation: &'static str,
        classify: fn(&NetError) -> Outcome,
        f: F,
    ) -> Result<T, NetError>
    where
        T: Send,
        F: Fn(Arc<C>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, NetError>> + Send,
    {
        let refusal = self
            .endpoints
            .iter()
            .find(|endpoint| endpoint.url() == url)
            .and_then(|endpoint| self.admissions.lock().get(url)?.refusal(endpoint));
        if let Some(error) = refusal {
            return Err(error);
        }
        let standing = |endpoint: &Endpoint| {
            if endpoint.url() == url { Standing::Preferred } else { Standing::Excluded }
        };
        self.walk(operation, classify, standing, Rotation::Once, f).await
    }

    /// [`Pool::broadcast_once`] against the endpoint `standing` ranks first.
    pub async fn broadcast_once_ranked<T, F, Fut>(
        &self,
        operation: &'static str,
        classify: fn(&NetError) -> Outcome,
        standing: fn(&Endpoint) -> Standing,
        f: F,
    ) -> Result<T, NetError>
    where
        T: Send,
        F: Fn(Arc<C>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, NetError>> + Send,
    {
        self.walk(operation, classify, standing, Rotation::Once, f).await
    }

    async fn walk<T, F, Fut>(
        &self,
        operation: &'static str,
        classify: fn(&NetError) -> Outcome,
        standing: impl Fn(&Endpoint) -> Standing + Send,
        rotation: Rotation,
        f: F,
    ) -> Result<T, NetError>
    where
        T: Send,
        F: Fn(Arc<C>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, NetError>> + Send,
    {
        let ordered = self.ordered(standing);
        if ordered.is_empty() {
            return Err(NetError::NoEndpoints { chain: self.chain });
        }
        let deadline = self.clock.deadline_in(self.timeouts.walk);
        let mut attempts: Vec<EndpointAttempt> = Vec::new();
        let mut has_tried = false;
        let mut ready_in: Option<Duration> = None;

        for endpoint in ordered {
            if has_tried && rotation == Rotation::Once {
                break;
            }
            if deadline.remaining(self.clock.as_ref()) < self.timeouts.floor {
                break;
            }
            if self.is_disqualified(endpoint) {
                continue;
            }
            let permit = match self.pacer.try_acquire(endpoint.url()) {
                Ok(permit) => permit,
                Err(wait) => {
                    ready_in = Some(ready_in.map_or(wait, |current| current.min(wait)));
                    continue;
                }
            };
            has_tried = true;

            // A turn gets the smaller of what the walk has left and one attempt's own
            // budget, and everything the turn does comes out of it, because independent
            // timeouts add up while one shared deadline does not.
            let turn = deadline.min(self.clock.deadline_in(self.timeouts.per_attempt));
            let started_millis = self.clock.elapsed_millis();
            let result = self.attempt(endpoint, turn, &f).await;
            drop(permit);
            let (outcome, error) = match result {
                Ok(value) => {
                    self.credit(endpoint, started_millis);
                    return Ok(value);
                }
                Err(Failed::Rotate(error)) => (Outcome::Rotate, error),
                Err(Failed::Classify(error)) => (classify(&error), error),
            };
            match outcome {
                Outcome::Accept => {
                    self.credit(endpoint, started_millis);
                    return Err(error);
                }
                Outcome::Fail => return Err(error),
                Outcome::Rotate => attempts.push(self.penalize(endpoint, operation, &error)),
                Outcome::Skip => attempts.push(EndpointAttempt::new(
                    endpoint.label().clone(),
                    error.kind(),
                    reason(&error),
                )),
            }
        }

        if attempts.is_empty() {
            let ready_in_millis = ready_in.map(duration_millis);
            tracing::debug!(
                target: TARGET,
                chain = self.chain.as_str(),
                operation,
                ready_in_millis,
                "no endpoint was ready, so nothing was sent"
            );
            return Err(NetError::NoneReady { chain: self.chain, operation });
        }
        let reasons: Vec<&str> = attempts.iter().map(|attempt| attempt.message.as_str()).collect();
        tracing::warn!(
            target: TARGET,
            chain = self.chain.as_str(),
            operation,
            attempts = attempts.len(),
            reasons = %reasons.join("; "),
            "no endpoint answered"
        );
        Err(NetError::AllFailed { chain: self.chain, operation, attempts })
    }

    /// One endpoint's whole turn, which connects and proves identity if needed and then
    /// runs the caller's closure. Every step comes out of `turn`.
    async fn attempt<T, F, Fut>(
        &self,
        endpoint: &Endpoint,
        turn: Deadline,
        f: &F,
    ) -> Result<T, Failed>
    where
        F: Fn(Arc<C>) -> Fut + Send + Sync,
        Fut: Future<Output = Result<T, NetError>> + Send,
    {
        let connection = self.connection(endpoint, turn).await?;
        let left = turn.remaining(self.clock.as_ref());
        within(endpoint.label(), left, f(connection)).await.map_err(Failed::Classify)
    }

    /// The cached connection for `endpoint`, or a new one that has proved which
    /// network it serves. Only a connection that got that far is cached.
    async fn connection(&self, endpoint: &Endpoint, turn: Deadline) -> Result<Arc<C>, Failed> {
        if let Some(Admission::Verified { connection: Some(open) }) =
            self.admissions.lock().get(endpoint.url())
        {
            return Ok(Arc::clone(open));
        }
        let connection = self.dial(endpoint, turn).await?;
        if self.is_unverified(endpoint) {
            self.verify(endpoint, &connection, turn).await?;
        }
        self.admit(endpoint, &connection)?;
        Ok(connection)
    }

    /// Records the endpoint as verified with its connection, unless another walk
    /// disqualified it while this one was dialling or checking. A mismatch holds for the
    /// rest of the session, so a confirmation that comes back after it changes nothing.
    fn admit(&self, endpoint: &Endpoint, connection: &Arc<C>) -> Result<(), Failed> {
        let mut admissions = self.admissions.lock();
        let admission = admissions
            .entry(endpoint.url().to_owned())
            .or_insert(Admission::Verified { connection: None });
        if let Some(error) = admission.refusal(endpoint) {
            return Err(Failed::Rotate(error));
        }
        *admission = Admission::Verified { connection: Some(Arc::clone(connection)) };
        Ok(())
    }

    /// A dial gets the transport's connect budget, or what is left of the turn
    /// if that is shorter.
    async fn dial(&self, endpoint: &Endpoint, turn: Deadline) -> Result<Arc<C>, Failed> {
        let budget = turn.remaining(self.clock.as_ref()).min(self.timeouts.connect);
        within(endpoint.label(), budget, self.connector.connect(endpoint))
            .await
            .map_err(Failed::Classify)
    }

    /// A check that fails rotates and a mismatch disqualifies, whatever the classifier
    /// would say. A failed check may be transient, so only a mismatch is remembered.
    async fn verify(
        &self,
        endpoint: &Endpoint,
        connection: &C,
        turn: Deadline,
    ) -> Result<(), Failed> {
        let budget = turn.remaining(self.clock.as_ref());
        let identity =
            within(endpoint.label(), budget, self.connector.identify(endpoint, connection))
                .await
                .map_err(Failed::Rotate)?;
        match identity {
            Identity::Mismatch { expected, found } => {
                self.disqualify(endpoint, &expected, &found);
                Err(Failed::Rotate(NetError::WrongNetwork {
                    endpoint: endpoint.label().clone(),
                    expected,
                    found,
                }))
            }
            Identity::Confirmed { detail } => {
                tracing::debug!(
                    target: TARGET,
                    chain = self.chain.as_str(),
                    endpoint = %endpoint.label(),
                    identity = detail,
                    "endpoint identity confirmed"
                );
                Ok(())
            }
            Identity::Unchecked => Ok(()),
        }
    }

    fn credit(&self, endpoint: &Endpoint, started_millis: u64) {
        let latency_millis = self.clock.elapsed_millis().saturating_sub(started_millis);
        self.pacer.record_success(endpoint.url());
        self.health.record_success(endpoint, Duration::from_millis(latency_millis));
    }

    /// Drops the cached connection, backs off the pacer, records the failure in health,
    /// and returns the attempt, whose message is for the log.
    fn penalize(
        &self,
        endpoint: &Endpoint,
        operation: &'static str,
        error: &NetError,
    ) -> EndpointAttempt {
        tracing::debug!(
            target: TARGET,
            chain = self.chain.as_str(),
            operation,
            endpoint = %endpoint.label(),
            kind = error.kind().as_str(),
            "rotating to the next endpoint"
        );
        self.drop_connection(endpoint);
        self.pacer.record_failure(endpoint.url(), error.retry_after());
        self.health.record_failure(endpoint, error);
        EndpointAttempt::new(endpoint.label().clone(), error.kind(), reason(error))
    }

    /// The configured endpoints `standing` admits, best first, by health class, then
    /// standing, then origin, then latency tier, then configured position.
    ///
    /// Health dominates origin, so a pinned endpoint that is down cannot block every
    /// request, but pinning wins among equally healthy endpoints. Health dominates
    /// standing too, except that an untried endpoint counts as healthy against it, so
    /// a fallback that has answered before cannot keep the requests a preferred
    /// endpoint was never asked for. When every endpoint is preferred, as for
    /// [`Pool::call`], the order is health class, origin, latency tier, then position.
    fn ordered(&self, standing: impl Fn(&Endpoint) -> Standing) -> Vec<&Endpoint> {
        let mut keyed: Vec<_> = self
            .endpoints
            .iter()
            .enumerate()
            .filter_map(|(index, endpoint)| {
                let place = standing(endpoint);
                if place == Standing::Excluded {
                    return None;
                }
                let (class, latency_tier) = self.health.rank(self.chain, endpoint.url());
                let settled =
                    if class == HealthClass::Unknown { HealthClass::Healthy } else { class };
                Some(((settled, place, class, endpoint.origin(), latency_tier, index), endpoint))
            })
            .collect();
        keyed.sort_unstable_by_key(|(key, _)| *key);
        keyed.into_iter().map(|(_, endpoint)| endpoint).collect()
    }

    fn is_unverified(&self, endpoint: &Endpoint) -> bool {
        !self.admissions.lock().contains_key(endpoint.url())
    }

    fn is_disqualified(&self, endpoint: &Endpoint) -> bool {
        matches!(self.admissions.lock().get(endpoint.url()), Some(Admission::Disqualified { .. }))
    }

    fn disqualify(&self, endpoint: &Endpoint, expected: &str, found: &str) {
        tracing::warn!(
            target: TARGET,
            chain = self.chain.as_str(),
            endpoint = %endpoint.label(),
            "endpoint serves another network and is disqualified for this session"
        );
        let disqualified =
            Admission::Disqualified { expected: expected.to_owned(), found: found.to_owned() };
        self.admissions.lock().insert(endpoint.url().to_owned(), disqualified);
    }

    fn drop_connection(&self, endpoint: &Endpoint) {
        if let Some(Admission::Verified { connection }) =
            self.admissions.lock().get_mut(endpoint.url())
        {
            *connection = None;
        }
    }
}

/// Why one endpoint failed, for the log line.
///
/// [`NetError::Endpoint`]'s `Display` names only the endpoint and the failure class and
/// keeps the protocol crate's error as its `source`, so the whole chain is rendered.
/// Every source boxed here is URL-free: reqwest's is stripped with `without_url`, and the
/// TLS, io and websocket errors name no address. A protocol crate that boxes its own
/// error must keep it so.
fn reason(error: &NetError) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(source) = cause {
        let message = source.to_string();
        if !message.is_empty() && !text.contains(&message) {
            text.push_str(": ");
            text.push_str(&message);
        }
        cause = source.source();
    }
    text
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use jelly_core::test_util::FakeClock;

    use super::*;
    use crate::pacer::PacerPolicy;

    const URL: &str = "https://a.example";

    #[derive(Debug)]
    struct Connection;

    /// Records every dial, so a test can tell whether anything was sent.
    #[derive(Debug, Default)]
    struct RecordingConnector {
        connects: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Connect<Connection> for RecordingConnector {
        async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<Connection>, NetError> {
            self.connects.lock().push(endpoint.url().to_owned());
            Ok(Arc::new(Connection))
        }

        async fn identify(
            &self,
            _endpoint: &Endpoint,
            _connection: &Connection,
        ) -> Result<Identity, NetError> {
            Ok(Identity::Unchecked)
        }
    }

    const fn rotate(_: &NetError) -> Outcome {
        Outcome::Rotate
    }

    #[tokio::test(start_paused = true)]
    async fn a_walk_with_every_endpoint_cooling_down_sends_nothing_and_says_so() {
        let policy = PacerPolicy {
            min_interval: Duration::from_millis(200),
            burst: 1,
            max_concurrent: 1,
            backoff_floor: Duration::from_secs(5),
            backoff_ceiling: Duration::from_secs(5),
        };
        let runtime = NetRuntime::with_pacing(Arc::new(FakeClock::default()), policy);
        let connector = Arc::new(RecordingConnector::default());
        let endpoint = Endpoint::parse(ChainId::Ethereum, Transport::HttpJsonrpc, URL)
            .expect("a canonical url");
        let pool = runtime.pool(
            ChainId::Ethereum,
            Transport::HttpJsonrpc,
            Arc::<RecordingConnector>::clone(&connector),
            vec![endpoint],
        );
        runtime.pacer().record_failure(URL, None);

        let error = pool
            .call("head", rotate, async |_connection| Ok(()))
            .await
            .expect_err("the only endpoint is cooling down");

        assert!(matches!(error, NetError::NoneReady { .. }), "{error}");
        assert!(connector.connects.lock().is_empty(), "nothing was sent");
        assert_eq!(
            runtime.health().entry(ChainId::Ethereum, URL).health.failures,
            0,
            "no endpoint's health moved"
        );
        assert!(runtime.pacer().ready_in(URL) > Duration::ZERO);
    }
}
