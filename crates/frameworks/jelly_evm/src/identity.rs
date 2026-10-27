use async_trait::async_trait;
use jelly_core::EndpointLabel;
use jelly_net::{
    Endpoint, HttpEndpoint, IDENTITY_TIMEOUT, Identity, IdentityCheck, JsonRpcRequest, NetError,
    WsClient,
};

use crate::method::Method;
use crate::params::EvmParams;

/// A JSON-RPC node must answer `eth_chainId` with the network's own id.
#[async_trait]
impl IdentityCheck<HttpEndpoint> for EvmParams {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &HttpEndpoint,
    ) -> Result<Identity, NetError> {
        let request = JsonRpcRequest::new(1, Method::ChainId.as_str(), &[(); 0])?;
        let hex: String = connection.rpc(&request, IDENTITY_TIMEOUT).await?;
        verdict(self, connection.label(), &hex)
    }
}

/// The same question over the socket, so a pool never waits on pushes from a host
/// that serves another network.
#[async_trait]
impl IdentityCheck<WsClient> for EvmParams {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &WsClient,
    ) -> Result<Identity, NetError> {
        let hex: String = connection.call(Method::ChainId.as_str(), &[(); 0]).await?;
        verdict(self, connection.endpoint(), &hex)
    }
}

fn verdict(params: &EvmParams, endpoint: &EndpointLabel, hex: &str) -> Result<Identity, NetError> {
    let expected = params.chain_id;
    let found = u64::from_str_radix(hex.strip_prefix("0x").unwrap_or(hex), 16)
        .map_err(|_| NetError::protocol(endpoint, "the chain id is not a number"))?;
    if found == expected {
        return Ok(Identity::confirmed(format!("chain id {found}")));
    }
    Ok(Identity::Mismatch { expected: expected.to_string(), found: found.to_string() })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jelly_core::{Clock, Transport, test_util::FakeClock};
    use jelly_net::{
        ProbeReport, probe,
        test_util::{FakeWs, WsAnswer},
    };

    use super::*;
    use crate::test_util::{ARBITRUM, AVALANCHE, BASE, BSC, ETHEREUM, OPTIMISM, POLYGON, endpoint};

    #[test]
    fn each_network_admits_its_own_chain_id_and_names_any_other() {
        assert_eq!(
            verdict(&ETHEREUM, &endpoint(), "0x1").unwrap(),
            Identity::confirmed("chain id 1")
        );
        assert_eq!(
            verdict(&ARBITRUM, &endpoint(), "0xa4b1").unwrap(),
            Identity::confirmed("chain id 42161")
        );
        assert_eq!(
            verdict(&BASE, &endpoint(), "0x2105").unwrap(),
            Identity::confirmed("chain id 8453")
        );
        assert_eq!(
            verdict(&OPTIMISM, &endpoint(), "0xa").unwrap(),
            Identity::confirmed("chain id 10")
        );
        assert_eq!(verdict(&BSC, &endpoint(), "0x38").unwrap(), Identity::confirmed("chain id 56"));
        assert_eq!(
            verdict(&POLYGON, &endpoint(), "0x89").unwrap(),
            Identity::confirmed("chain id 137")
        );
        assert_eq!(
            verdict(&AVALANCHE, &endpoint(), "0xa86a").unwrap(),
            Identity::confirmed("chain id 43114")
        );
        assert_eq!(
            verdict(&ETHEREUM, &endpoint(), "0x38").unwrap(),
            Identity::Mismatch { expected: "1".to_owned(), found: "56".to_owned() }
        );
    }

    #[test]
    fn an_answer_that_is_not_a_number_fails_rather_than_mismatching() {
        for answer in ["mainnet", "0x0x1"] {
            let error = verdict(&ETHEREUM, &endpoint(), answer).unwrap_err();
            assert!(matches!(error, NetError::Protocol { .. }), "{answer}: {error}");
        }
    }

    /// The probe runs the check the websocket pool is built with, so a socket on
    /// another network is reported rather than admitted.
    #[tokio::test]
    async fn a_websocket_probe_asks_for_the_chain_id_over_the_socket() {
        let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
        let fake = FakeWs::new();
        fake.always(Method::ChainId.as_str(), WsAnswer::Result(r#""0x38""#.to_owned()));

        for (params, expected) in
            [(ETHEREUM, ProbeReport::WrongNetwork), (BSC, ProbeReport::Verified { latency_ms: 0 })]
        {
            let endpoint =
                Endpoint::parse(params.chain, Transport::WsJsonrpc, "wss://ws.invalid").unwrap();
            let connector = fake.connector(Arc::clone(&clock), Arc::new(params));
            assert_eq!(
                probe(&connector, clock.as_ref(), &endpoint).await,
                expected,
                "{:?}",
                params.chain
            );
        }
        assert_eq!(fake.calls_to(Method::ChainId.as_str()).len(), 2);
    }
}
