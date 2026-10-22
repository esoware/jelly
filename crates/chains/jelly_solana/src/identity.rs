use async_trait::async_trait;
use jelly_net::{
    Endpoint, HttpEndpoint, IDENTITY_TIMEOUT, Identity, IdentityCheck, JsonRpcRequest, NetError,
    WsClient,
};

use crate::method::Method;

/// What `getGenesisHash` returns on mainnet-beta. Every test cluster has its own.
const GENESIS_HASH: &str = "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d";

/// Mainnet-beta's identity, which a node proves by reporting mainnet-beta's genesis
/// hash.
#[derive(Debug, Clone, Copy)]
pub struct SolanaIdentity;

#[async_trait]
impl IdentityCheck<HttpEndpoint> for SolanaIdentity {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &HttpEndpoint,
    ) -> Result<Identity, NetError> {
        let request = JsonRpcRequest::new(1, Method::GetGenesisHash.as_str(), &[(); 0])?;
        let found: String = connection.rpc(&request, IDENTITY_TIMEOUT).await?;
        Ok(verdict(found))
    }
}

/// A Solana websocket serves subscriptions only and answers `getGenesisHash` with
/// method not found, so it has no identity to check. Its pushes only wake a sync
/// that reads over the identity-checked HTTP pool.
#[async_trait]
impl IdentityCheck<WsClient> for SolanaIdentity {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        _client: &WsClient,
    ) -> Result<Identity, NetError> {
        Ok(Identity::Unchecked)
    }
}

fn verdict(found: String) -> Identity {
    if found == GENESIS_HASH {
        return Identity::confirmed(format!("solana genesis {GENESIS_HASH}"));
    }
    Identity::Mismatch { expected: GENESIS_HASH.to_owned(), found }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jelly_core::{ChainId, Clock, Transport, test_util::FakeClock};
    use jelly_net::{
        ProbeReport, probe,
        test_util::{FakeWs, WsAnswer},
    };

    use super::*;

    #[test]
    fn only_mainnet_betas_genesis_hash_is_admitted() {
        assert_eq!(
            verdict(GENESIS_HASH.to_owned()),
            Identity::confirmed(format!("solana genesis {GENESIS_HASH}"))
        );
        let other = "11111111111111111111111111111111";
        assert_eq!(
            verdict(other.to_owned()),
            Identity::Mismatch { expected: GENESIS_HASH.to_owned(), found: other.to_owned() }
        );
    }

    #[tokio::test]
    async fn a_websocket_probe_sends_nothing_and_reports_the_socket_reachable() {
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
        let fake = FakeWs::new();
        fake.always(
            Method::GetGenesisHash.as_str(),
            WsAnswer::Error { code: -32601, message: "Method not found".to_owned() },
        );
        let endpoint =
            Endpoint::parse(ChainId::Solana, Transport::WsJsonrpc, "wss://ws.invalid").unwrap();
        let connector = fake.connector(Arc::clone(&clock), Arc::new(SolanaIdentity));

        assert_eq!(
            probe(&connector, clock.as_ref(), &endpoint).await,
            ProbeReport::Reachable { latency_ms: 0 }
        );
        assert!(fake.calls().is_empty(), "{:?}", fake.calls());
    }
}
