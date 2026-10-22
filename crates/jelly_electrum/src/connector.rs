use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::EndpointLabel;
use jelly_net::{Connect, Dialler, Endpoint, Identity, NetError, Socket};

use crate::chain::ElectrumChain;
use crate::client::ElectrumClient;
use crate::error::ElectrumError;

/// Builds the Electrum client a failover pool caches for each endpoint, and is what
/// the endpoint probe connects through too.
///
/// It carries the chain its caller expects, which every socket proves before it
/// serves.
#[derive(Debug, Clone)]
pub struct ElectrumConnector {
    dialler: Dialler,
    chain: ElectrumChain,
}

impl ElectrumConnector {
    /// `dialler` is the pinned one [`jelly_net::Network::electrum_dialler`] builds.
    pub const fn new(dialler: Dialler, chain: ElectrumChain) -> Self {
        Self { dialler, chain }
    }
}

#[async_trait]
impl Connect<ElectrumClient> for ElectrumConnector {
    async fn connect(&self, endpoint: &Endpoint) -> Result<Arc<ElectrumClient>, NetError> {
        let sockets =
            Arc::new(TlsSockets { dialler: self.dialler.clone(), endpoint: endpoint.clone() });
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

/// Opens fresh sockets to one endpoint. An [`ElectrumClient`] asks it for a new
/// socket whenever the last one went away.
///
/// Production passes TLS from [`Dialler`], and a test passes an in-memory pipe so
/// the shipped framing, batching, and decoding run without a network.
#[async_trait]
pub(crate) trait SocketSource: Send + Sync + fmt::Debug {
    /// The redacted endpoint every failure on these sockets is attributed to.
    fn endpoint(&self) -> &EndpointLabel;

    async fn open(&self) -> Result<Box<dyn Socket>, ElectrumError>;
}

/// TLS-over-TCP sockets to one host, through the one dialler in the workspace.
#[derive(Debug)]
struct TlsSockets {
    dialler: Dialler,
    endpoint: Endpoint,
}

#[async_trait]
impl SocketSource for TlsSockets {
    fn endpoint(&self) -> &EndpointLabel {
        self.endpoint.label()
    }

    async fn open(&self) -> Result<Box<dyn Socket>, ElectrumError> {
        Ok(self.dialler.connect(&self.endpoint).await?)
    }
}
