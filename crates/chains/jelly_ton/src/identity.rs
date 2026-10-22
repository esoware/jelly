use async_trait::async_trait;
use jelly_net::{Endpoint, HttpEndpoint, IDENTITY_TIMEOUT, Identity, IdentityCheck, NetError};
use serde_json::json;

use crate::config::is_mainnet;
use crate::method::Method;
use crate::node::request;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TonIdentity;

#[async_trait]
impl IdentityCheck<HttpEndpoint> for TonIdentity {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &HttpEndpoint,
    ) -> Result<Identity, NetError> {
        let result =
            request(connection, Method::GetMasterchainInfo, &json!({}), IDENTITY_TIMEOUT).await?;
        let Some(is_mainnet) = is_mainnet(&result) else {
            return Err(NetError::protocol(connection.label(), "missing TON network identity"));
        };
        if is_mainnet {
            Ok(Identity::confirmed("TON mainnet"))
        } else {
            Ok(Identity::Mismatch {
                expected: String::from("TON mainnet"),
                found: String::from("another TON network"),
            })
        }
    }
}
