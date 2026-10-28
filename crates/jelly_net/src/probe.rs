//! The endpoint editor's probe: the pool's identity check, run once on demand.
//!
//! The caller passes the connector the endpoint's own pool uses, so the probe and the
//! pool ask the same question.

use std::time::Duration;

use jelly_core::{Clock, EndpointLabel, FailureKind, wire_enum};
use serde::Serialize;

use crate::connect::{Connect, Identity};
use crate::endpoint::{Endpoint, UrlRejection};
use crate::error::{NetError, within};
use crate::http::{HttpClient, HttpRequest};

const TARGET: &str = "jelly::net::probe";

/// Budget for one whole probe, the dial and the identity check together.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_PROBE_BYTES: usize = 64 * 1024;

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Why a probe got no usable answer. The interface words each one itself.
    pub enum ProbeFailure {
        TimedOut => "timed_out",
        /// The connection couldn't be opened.
        NoConnection => "no_connection",
        /// The TLS handshake failed or the certificate didn't verify.
        Tls => "tls",
        /// The connection closed before an answer arrived.
        Dropped => "dropped",
        /// The endpoint answered with an HTTP error status.
        HttpStatus => "http_status",
        RateLimited => "rate_limited",
        /// The endpoint answered with something this build can't use.
        BadAnswer => "bad_answer",
        /// This build can't check the endpoint, so nothing was asked.
        NotProbed => "not_probed",
    }
}

/// What one probe found.
///
/// `Reachable` means there was no network to identify, as for the REST helper services.
/// `WrongNetwork` leaves out what the host reported, a raw wire value that only the log
/// gets.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProbeReport {
    #[serde(rename_all = "camelCase")]
    Verified {
        latency_ms: u64,
    },
    #[serde(rename_all = "camelCase")]
    Reachable {
        latency_ms: u64,
    },
    WrongNetwork,
    #[serde(rename_all = "camelCase")]
    Unreachable {
        failure: ProbeFailure,
    },
}

impl ProbeReport {
    /// The answer for an endpoint on a transport the chain builds no pool for. The
    /// registry never lists one, so only an endpoint built outside it gets here, and it
    /// gets the registry's own refusal.
    pub fn unused_transport(endpoint: &EndpointLabel) -> Self {
        Self::from_failure(
            endpoint,
            &NetError::InvalidUrl { reason: UrlRejection::UnusedTransport },
        )
    }

    /// The report for a failure. Its text goes to the log, and the report carries only
    /// its cause.
    pub fn from_failure(endpoint: &EndpointLabel, error: &NetError) -> Self {
        tracing::info!(target: TARGET, %endpoint, %error, "the probe failed");
        let failure = match *error {
            NetError::WrongNetwork { .. } => return Self::WrongNetwork,
            NetError::Endpoint { kind, .. } => match kind {
                FailureKind::Connect => ProbeFailure::NoConnection,
                FailureKind::Tls => ProbeFailure::Tls,
                FailureKind::Timeout => ProbeFailure::TimedOut,
                FailureKind::Transport => ProbeFailure::Dropped,
                FailureKind::Protocol => ProbeFailure::BadAnswer,
                FailureKind::RateLimited => ProbeFailure::RateLimited,
                FailureKind::Server => ProbeFailure::HttpStatus,
                FailureKind::Client => ProbeFailure::NotProbed,
            },
            NetError::Timeout { .. } => ProbeFailure::TimedOut,
            NetError::Status { .. } => ProbeFailure::HttpStatus,
            NetError::RateLimited { .. } => ProbeFailure::RateLimited,
            NetError::Protocol { .. } | NetError::TooLarge { .. } | NetError::Rpc { .. } => {
                ProbeFailure::BadAnswer
            }
            NetError::NoEndpoints { .. }
            | NetError::NoneReady { .. }
            | NetError::AllFailed { .. }
            | NetError::InvalidUrl { .. }
            | NetError::Defaults { .. }
            | NetError::Config { .. } => ProbeFailure::NotProbed,
        };
        Self::Unreachable { failure }
    }
}

/// Connects to one endpoint through `connector` and asks which network it serves.
///
/// A pool runs the same two steps before an endpoint's first request, so `connector` must
/// be the one the endpoint's pool is built with. A mismatch is only reported, and
/// disqualifies nothing.
pub async fn probe<C, K>(connector: &K, clock: &dyn Clock, endpoint: &Endpoint) -> ProbeReport
where
    C: Send + Sync,
    K: Connect<C>,
{
    let started_millis = clock.elapsed_millis();
    let run = async {
        let connection = connector.connect(endpoint).await?;
        connector.identify(endpoint, &connection).await
    };
    let outcome = within(endpoint.label(), PROBE_TIMEOUT, run).await;
    report(endpoint, outcome, clock.elapsed_millis().saturating_sub(started_millis))
}

/// The probe for the REST helper services, which have no network to identify, so the only
/// question is whether the host answers.
///
/// It sends one GET to the endpoint's own address, adding no path. Any status below 500
/// counts as an answer.
pub async fn probe_reachable(
    http: &HttpClient,
    clock: &dyn Clock,
    endpoint: &Endpoint,
) -> ProbeReport {
    let started_millis = clock.elapsed_millis();
    let request = HttpRequest::get(PROBE_TIMEOUT).with_max_bytes(MAX_PROBE_BYTES);
    let outcome = match http.send(endpoint, "", request).await {
        Ok(response) if response.status() < 500 => Ok(Identity::Unchecked),
        Ok(response) => {
            Err(NetError::Status { endpoint: endpoint.label().clone(), status: response.status() })
        }
        Err(error) => Err(error),
    };
    report(endpoint, outcome, clock.elapsed_millis().saturating_sub(started_millis))
}

fn report(
    endpoint: &Endpoint,
    outcome: Result<Identity, NetError>,
    latency_ms: u64,
) -> ProbeReport {
    match outcome {
        Ok(Identity::Confirmed { .. }) => ProbeReport::Verified { latency_ms },
        Ok(Identity::Unchecked) => ProbeReport::Reachable { latency_ms },
        Ok(Identity::Mismatch { expected, found }) => {
            tracing::info!(
                target: TARGET,
                endpoint = %endpoint.label(),
                %expected,
                %found,
                "the probe found another network"
            );
            ProbeReport::WrongNetwork
        }
        Err(error) => ProbeReport::from_failure(endpoint.label(), &error),
    }
}
