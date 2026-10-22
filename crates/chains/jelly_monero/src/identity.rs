use jelly_core::EndpointLabel;
use jelly_net::{
    Endpoint, HttpClient, HttpRequest, IDENTITY_TIMEOUT, Identity, JsonRpcRequest, NetError,
    json_rpc_decode,
};
use serde::Deserialize;

/// The network type monerod names when it runs on mainnet.
const MAINNET: &str = "mainnet";

#[derive(Debug, Deserialize)]
struct Info {
    status: String,
    nettype: Option<String>,
    height: Option<u64>,
}

/// Asks monerod which network it runs, under the identity budget rather than the
/// slower one the negotiated daemon's requests get.
pub(crate) async fn identify(
    client: &HttpClient,
    endpoint: &Endpoint,
) -> Result<Identity, NetError> {
    let request = JsonRpcRequest::new(1, "get_info", &[(); 0])?;
    let body = client
        .send(endpoint, "/json_rpc", HttpRequest::post_json(request.as_str(), IDENTITY_TIMEOUT))
        .await?
        .into_body()?;
    let info: Info = json_rpc_decode(endpoint.label(), &body)?;
    verdict(endpoint.label(), &info)
}

/// Only an explicit mainnet confirms. monerod has reported `nettype` since 2018, so a
/// daemon that leaves it out proves nothing about its network.
fn verdict(endpoint: &EndpointLabel, info: &Info) -> Result<Identity, NetError> {
    if info.status != "OK" {
        return Err(NetError::protocol(endpoint, "get_info did not report OK"));
    }
    match info.nettype.as_deref() {
        Some(MAINNET) => Ok(Identity::confirmed(format!(
            "monero mainnet at height {}",
            info.height.unwrap_or_default()
        ))),
        Some(other) => {
            Ok(Identity::Mismatch { expected: MAINNET.to_owned(), found: other.to_owned() })
        }
        None => Err(NetError::protocol(endpoint, "the daemon did not report its network type")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::label;

    fn info(json: &str) -> Info {
        serde_json::from_str(json).expect("the fixture parses")
    }

    #[test]
    fn a_mainnet_daemon_is_confirmed_and_any_other_network_is_named() {
        assert_eq!(
            verdict(&label(), &info(r#"{"status":"OK","nettype":"mainnet","height":3000000}"#))
                .unwrap(),
            Identity::confirmed("monero mainnet at height 3000000")
        );
        assert_eq!(
            verdict(&label(), &info(r#"{"status":"OK","nettype":"stagenet","height":1}"#)).unwrap(),
            Identity::Mismatch { expected: "mainnet".to_owned(), found: "stagenet".to_owned() }
        );
    }

    #[test]
    fn a_daemon_that_names_no_network_fails_rather_than_confirming_or_mismatching() {
        for body in [r#"{"status":"OK"}"#, r#"{"status":"OK","nettype":null,"height":3000000}"#] {
            let error =
                verdict(&label(), &info(body)).expect_err("a missing network proves nothing");
            assert!(
                matches!(
                    error,
                    NetError::Protocol { detail: "the daemon did not report its network type", .. }
                ),
                "{body}: {error}"
            );
        }
    }

    #[test]
    fn a_daemon_that_does_not_report_ok_fails_rather_than_confirming() {
        let error = verdict(&label(), &info(r#"{"status":"BUSY","nettype":"mainnet"}"#))
            .expect_err("a busy daemon proves nothing");
        assert!(matches!(error, NetError::Protocol { .. }), "{error}");
    }
}
