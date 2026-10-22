use async_trait::async_trait;
use jelly_net::{Endpoint, HttpEndpoint, IDENTITY_TIMEOUT, Identity, IdentityCheck, NetError};
use serde_json::Value;

use crate::config::is_mainnet_status;
use crate::node::read;
use crate::route::Route;

const MAINNET_GENESIS_HASH: &str =
    "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08";

#[derive(Debug, Clone, Copy)]
pub(crate) struct ZcashIdentity;

#[async_trait]
impl IdentityCheck<HttpEndpoint> for ZcashIdentity {
    async fn identify(
        &self,
        _endpoint: &Endpoint,
        connection: &HttpEndpoint,
    ) -> Result<Identity, NetError> {
        let status = read(connection, Route::Status, IDENTITY_TIMEOUT).await?;
        let genesis = read(connection, Route::BlockHash(0), IDENTITY_TIMEOUT).await?;
        if is_mainnet_status(&status)
            && genesis.get("blockHash").and_then(Value::as_str) == Some(MAINNET_GENESIS_HASH)
        {
            Ok(Identity::confirmed("Zcash mainnet"))
        } else {
            Ok(Identity::Mismatch {
                expected: String::from("Zcash mainnet"),
                found: String::from("another network"),
            })
        }
    }
}
