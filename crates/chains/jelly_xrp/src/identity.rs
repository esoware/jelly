use async_trait::async_trait;
use jelly_net::{Endpoint, HttpEndpoint, IDENTITY_TIMEOUT, Identity, IdentityCheck, NetError};
use serde_json::{Value, json};

use crate::method::Method;
use crate::node::request;

#[derive(Debug, Clone, Copy)]
pub(crate) struct XrpIdentity;

#[async_trait]
impl IdentityCheck<HttpEndpoint> for XrpIdentity {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &HttpEndpoint,
    ) -> Result<Identity, NetError> {
        let result = request(connection, Method::ServerInfo, &json!({}), IDENTITY_TIMEOUT).await?;
        let network_id =
            result.pointer("/info/network_id").and_then(Value::as_u64).ok_or_else(|| {
                NetError::protocol(connection.label(), "missing XRP network identity")
            })?;
        if network_id == 0 {
            Ok(Identity::confirmed("XRP Ledger mainnet"))
        } else {
            Ok(Identity::Mismatch {
                expected: String::from("XRP Ledger mainnet"),
                found: format!("network {network_id}"),
            })
        }
    }
}
