use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::endpoint::Endpoint;
use crate::error::NetError;

/// What a connector found out about the network behind an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// This host serves the network the chain expects, with a short
    /// human-readable statement of what it reported.
    Confirmed { detail: String },
    /// This transport has no network identity to check, so reaching it is all
    /// there is to know.
    Unchecked,
    /// The host serves a different network. It is disqualified for the rest of
    /// the session rather than allowed to poison sync state.
    Mismatch { expected: String, found: String },
}

impl Identity {
    pub fn confirmed(detail: impl Into<String>) -> Self {
        Self::Confirmed { detail: detail.into() }
    }
}

/// The budget for the one request an identity check sends, so a slow answer
/// cannot spend a whole attempt.
pub const IDENTITY_TIMEOUT: Duration = Duration::from_secs(10);

/// Builds whatever a chain talks to an endpoint through, such as an HTTP handle, an
/// Electrum socket, or an RPC client.
///
/// The pool bounds each method with what is left of the attempt, and [`crate::probe()`]
/// bounds both with the probe's budget, so an inner timeout such as [`IDENTITY_TIMEOUT`]
/// only stops one step from spending the whole attempt.
#[async_trait]
pub trait Connect<C>: Send + Sync + fmt::Debug {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<C>, NetError>;

    /// Asks the endpoint which network it serves, once per endpoint per session before
    /// its first request. An [`Identity::Mismatch`] disqualifies the endpoint for the
    /// rest of the session.
    async fn identify(&self, endpoint: &Endpoint, connection: &C) -> Result<Identity, NetError>;
}

/// A chain's own answer to which network an endpoint serves.
///
/// [`crate::HttpConnector`] and [`crate::WsConnector`] reach every chain, so they hold
/// no network facts. The chain's crate supplies the question and the expected answer
/// here, and the connector runs it as its [`Connect::identify`].
#[async_trait]
pub trait IdentityCheck<C>: Send + Sync + fmt::Debug {
    async fn identify(&self, endpoint: &Endpoint, connection: &C) -> Result<Identity, NetError>;
}

/// The check for the two REST transports, which are helper services with no network
/// to identify.
///
/// It sends nothing, because there is nothing to compare and the first real request
/// shows whether the host is up.
#[derive(Debug, Clone, Copy)]
pub struct NoIdentity;

#[async_trait]
impl<C: Send + Sync + 'static> IdentityCheck<C> for NoIdentity {
    async fn identify(&self, _endpoint: &Endpoint, _connection: &C) -> Result<Identity, NetError> {
        Ok(Identity::Unchecked)
    }
}
