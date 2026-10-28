use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::EndpointLabel;
use jelly_net::{Connect, Dialer, Endpoint, Identity, NetError, Socket};

use crate::chain::ElectrumChain;
use crate::client::ElectrumClient;
use crate::error::ElectrumError;

/// Builds the client a pool caches for each endpoint. A probe connects through it too.
///
/// Every socket must prove the expected chain before it serves.
#[derive(Debug, Clone)]
pub struct ElectrumConnector {
    dialer: Dialer,
    chain: ElectrumChain,
}

impl ElectrumConnector {
    /// `dialer` is the pinned one [`jelly_net::Network::electrum_dialer`] builds.
    pub const fn new(dialer: Dialer, chain: ElectrumChain) -> Self {
        Self { dialer, chain }
    }
}

#[async_trait]
impl Connect<ElectrumClient> for ElectrumConnector {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<ElectrumClient>, NetError> {
        let sockets =
            Arc::new(TlsSockets { dialer: self.dialer.clone(), endpoint: endpoint.clone() });
        let client = ElectrumClient::open(sockets, self.chain).await?;
        Ok(Arc::new(client))
    }

    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &ElectrumClient,
    ) -> Result<Identity, NetError> {
        Ok(connection.verify_identity().await?)
    }
}

/// Opens a fresh socket to one endpoint whenever an [`ElectrumClient`] loses its last one.
///
/// Production dials TLS through [`Dialer`]. Tests pass an in-memory pipe, so the real
/// framing, batching, and decoding run without a network.
#[async_trait]
pub(crate) trait SocketSource: Send + Sync + fmt::Debug {
    /// The redacted endpoint every failure on these sockets is attributed to.
    fn endpoint(&self) -> &EndpointLabel;

    async fn open(&self) -> Result<Box<dyn Socket>, ElectrumError>;
}

#[derive(Debug)]
struct TlsSockets {
    dialer: Dialer,
    endpoint: Endpoint,
}

#[async_trait]
impl SocketSource for TlsSockets {
    fn endpoint(&self) -> &EndpointLabel {
        self.endpoint.label()
    }

    async fn open(&self) -> Result<Box<dyn Socket>, ElectrumError> {
        Ok(self.dialer.connect(&self.endpoint).await?)
    }
}
