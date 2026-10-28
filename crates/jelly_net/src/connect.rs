use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use crate::endpoint::Endpoint;
use crate::error::NetError;

/// What a connector found out about the network behind an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Identity {
    /// The host serves the expected network. `detail` says what it reported, for the log.
    Confirmed { detail: String },
    /// The transport has no network identity to check.
    Unchecked,
    /// The host serves another network, so the pool disqualifies it for the session.
    Mismatch { expected: String, found: String },
}

impl Identity {
    pub fn confirmed(detail: impl Into<String>) -> Self {
        Self::Confirmed { detail: detail.into() }
    }
}

/// Bounds the one request an identity check sends, so a slow answer can't spend a whole
/// attempt.
pub const IDENTITY_TIMEOUT: Duration = Duration::from_secs(10);

/// Opens whatever a chain talks to an endpoint through, such as an HTTP handle, an
/// Electrum socket, or an RPC client.
///
/// The pool bounds each method by what's left of the attempt, and [`crate::probe()`] by
/// the probe's budget. An inner timeout such as [`IDENTITY_TIMEOUT`] only keeps one step
/// from using up the rest.
#[async_trait]
pub trait Connect<C>: Send + Sync + fmt::Debug {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<C>, NetError>;

    /// Asks which network the endpoint serves, once per session before its first request.
    /// An [`Identity::Mismatch`] disqualifies the endpoint for the rest of the session.
    async fn identify(&self, endpoint: &Endpoint, connection: &C) -> Result<Identity, NetError>;
}

/// A chain's check of which network an endpoint serves, which the shared
/// [`crate::HttpConnector`] and [`crate::WsConnector`] run as their [`Connect::identify`].
#[async_trait]
pub trait IdentityCheck<C>: Send + Sync + fmt::Debug {
    async fn identify(&self, endpoint: &Endpoint, connection: &C) -> Result<Identity, NetError>;
}

/// The check for the REST helper services, which serve no network to identify.
///
/// It sends nothing. The first real request shows whether the host is up.
#[derive(Debug, Clone, Copy)]
pub struct NoIdentity;

#[async_trait]
impl<C: Send + Sync + 'static> IdentityCheck<C> for NoIdentity {
    async fn identify(&self, _endpoint: &Endpoint, _connection: &C) -> Result<Identity, NetError> {
        Ok(Identity::Unchecked)
    }
}
