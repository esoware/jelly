use async_trait::async_trait;
use jelly_core::EndpointLabel;
use jelly_net::{
    Endpoint, HttpEndpoint, HttpRequest, IDENTITY_TIMEOUT, Identity, IdentityCheck, NetError,
};
use serde::Deserialize;

const TARGET: &str = "jelly::tron::identity";

/// The `blockID` of block zero on TRON mainnet. Its last four bytes are what TRON's
/// `eth_chainId` answers.
const GENESIS_BLOCK_ID: &str = "00000000000000001ebf88508a03865c71d452e25f4d51194196a1d22b6653dc";

/// Checks that a node reports mainnet's block zero.
///
/// TRON's HTTP API has no chain-id call, and Nile and Shasta each have their own
/// genesis.
#[derive(Debug, Clone, Copy)]
pub struct TronIdentity;

#[async_trait]
impl IdentityCheck<HttpEndpoint> for TronIdentity {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &HttpEndpoint,
    ) -> Result<Identity, NetError> {
        let request = HttpRequest::post_json(r#"{"num":0}"#, IDENTITY_TIMEOUT);
        let body = connection.send("/wallet/getblockbynum", request).await?.into_body()?;
        verdict(connection.label(), &body)
    }
}

#[derive(Debug, Deserialize)]
struct BlockZero {
    #[serde(rename = "blockID")]
    block_id: String,
}

fn verdict(endpoint: &EndpointLabel, body: &str) -> Result<Identity, NetError> {
    let block: BlockZero = serde_json::from_str(body).map_err(|source| {
        // By position, because serde's message quotes the answer back.
        tracing::debug!(
            target: TARGET,
            endpoint = %endpoint,
            line = source.line(),
            column = source.column(),
            "block zero does not parse"
        );
        NetError::protocol(endpoint, "the answer carries no block id")
    })?;
    if block.block_id == GENESIS_BLOCK_ID {
        return Ok(Identity::confirmed(format!("tron genesis {GENESIS_BLOCK_ID}")));
    }
    Ok(Identity::Mismatch { expected: GENESIS_BLOCK_ID.to_owned(), found: block.block_id })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::endpoint;

    #[test]
    fn only_mainnets_block_zero_is_admitted() {
        let mainnet = format!(r#"{{"blockID":"{GENESIS_BLOCK_ID}","block_header":{{}}}}"#);
        assert_eq!(
            verdict(&endpoint(), &mainnet).unwrap(),
            Identity::confirmed(format!("tron genesis {GENESIS_BLOCK_ID}"))
        );

        let other = "0".repeat(64);
        assert_eq!(
            verdict(&endpoint(), &format!(r#"{{"blockID":"{other}"}}"#)).unwrap(),
            Identity::Mismatch { expected: GENESIS_BLOCK_ID.to_owned(), found: other }
        );
    }

    #[test]
    fn a_body_without_a_block_id_fails_rather_than_mismatching() {
        for body in ["{}", r#"{"Error":"class org.tron.core Exception"}"#, "not json"] {
            let error = verdict(&endpoint(), body).unwrap_err();
            assert!(matches!(error, NetError::Protocol { .. }), "{body}: {error}");
        }
    }
}
