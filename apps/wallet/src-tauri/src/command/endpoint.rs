use jelly_chains::probe;
use jelly_core::{ChainError, ChainId, ErrorCode, Transport};
use jelly_net::{
    Endpoint, EndpointOrigin, EndpointStatus, NetError, Network, PlaintextPolicy, ProbeReport,
};
use serde::Serialize;
use specta::Type;
use tauri::State;

use crate::command::{TARGET, with_services};
use crate::error::IpcError;
use crate::state::AppState;

/// `url` is the canonical key the other endpoint commands take, and a pinned endpoint's
/// `origin` is `Pinned`. Health stays in Rust, and `probe_endpoint` checks an endpoint on
/// request.
#[derive(Debug, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EndpointRecord {
    url: String,
    transport: Transport,
    origin: EndpointOrigin,
    note: Option<String>,
    is_enabled: bool,
}

impl From<&EndpointStatus> for EndpointRecord {
    fn from(status: &EndpointStatus) -> Self {
        Self {
            url: status.endpoint.url().to_owned(),
            transport: status.endpoint.transport(),
            origin: status.endpoint.origin(),
            note: status.endpoint.note().map(ToOwned::to_owned),
            is_enabled: status.is_enabled,
        }
    }
}

/// Includes disabled endpoints, pinned first, then custom, then shipped.
#[tauri::command]
#[specta::specta]
pub(crate) async fn list_endpoints(
    state: State<'_, AppState>,
    chain: ChainId,
) -> Result<Vec<EndpointRecord>, IpcError> {
    with_services(&state, async move |services| {
        let rows = services.network().registry().configured(chain).map_err(unreadable)?;
        Ok(rows.iter().map(EndpointRecord::from).collect())
    })
    .await
}

/// Adds or re-enables an endpoint without changing its pin. A URL that's already listed
/// isn't added twice.
///
/// Plaintext outside loopback needs `should_allow_insecure`. Electrum refuses plaintext even
/// then.
#[tauri::command]
#[specta::specta]
pub(crate) async fn add_endpoint(
    state: State<'_, AppState>,
    chain: ChainId,
    url: String,
    should_allow_insecure: bool,
) -> Result<(), IpcError> {
    let plaintext = if should_allow_insecure {
        PlaintextPolicy::Allowed
    } else {
        PlaintextPolicy::LoopbackOnly
    };
    with_services(&state, async move |services| {
        // The canonical URL is the key the registry, pools, and database share.
        let endpoint =
            services.network().registry().resolve(chain, &url, plaintext).map_err(url_failure)?;
        let canonical_url = endpoint.url().to_owned();
        let display = endpoint.input().to_owned();
        services.engine()?.add_endpoint(chain, canonical_url, display).await?;
        services.endpoints_changed().await
    })
    .await
}

/// Deletes the endpoint's override, which removes a custom endpoint and restores a shipped
/// one's defaults.
#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_endpoint_override(
    state: State<'_, AppState>,
    chain: ChainId,
    url: String,
) -> Result<(), IpcError> {
    with_services(&state, async move |services| {
        let canonical_url = find_listed(services.network(), chain, &url)?.url().to_owned();
        services.engine()?.delete_endpoint(chain, canonical_url).await?;
        services.endpoints_changed().await
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_endpoint_enabled(
    state: State<'_, AppState>,
    chain: ChainId,
    url: String,
    is_enabled: bool,
) -> Result<(), IpcError> {
    with_services(&state, async move |services| {
        let canonical_url = find_listed(services.network(), chain, &url)?.url().to_owned();
        services.engine()?.set_endpoint_enabled(chain, canonical_url, is_enabled).await?;
        services.endpoints_changed().await
    })
    .await
}

/// A pinned endpoint is listed first and tried first among equally healthy ones.
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_endpoint_pinned(
    state: State<'_, AppState>,
    chain: ChainId,
    url: String,
    is_pinned: bool,
) -> Result<(), IpcError> {
    with_services(&state, async move |services| {
        let canonical_url = find_listed(services.network(), chain, &url)?.url().to_owned();
        services.engine()?.set_endpoint_pinned(chain, canonical_url, is_pinned).await?;
        services.endpoints_changed().await
    })
    .await
}

/// Checks which network the endpoint serves. A REST service is only checked for an answer.
#[tauri::command]
#[specta::specta]
pub(crate) async fn probe_endpoint(
    state: State<'_, AppState>,
    chain: ChainId,
    url: String,
) -> Result<ProbeReport, IpcError> {
    with_services(&state, async move |services| {
        // The network outlives shutdown, so only the engine handle says we're shutting down.
        services.engine()?;
        let endpoint = find_listed(services.network(), chain, &url)?;
        Ok(probe(services.network(), &endpoint).await)
    })
    .await
}

/// Refuses a URL that isn't already listed. Otherwise a script in the page could probe any
/// host, or add one through the upsert behind the enable and pin commands and skip the
/// checks in `add_endpoint`.
fn find_listed(network: &Network, chain: ChainId, url: &str) -> Result<Endpoint, IpcError> {
    network.registry().find(chain, url).map_err(unreadable)?.ok_or_else(|| {
        IpcError::new(ErrorCode::NotFound.into(), "That endpoint is not in the list.")
    })
}

fn url_failure(error: NetError) -> IpcError {
    if let NetError::InvalidUrl { reason } = error {
        IpcError::invalid_input(reason.message())
    } else {
        unreadable(error)
    }
}

/// Logs the redacted error. The code and copy come from the `NetError` conversion.
fn unreadable(error: NetError) -> IpcError {
    tracing::error!(target: TARGET, %error, "the endpoint registry couldn't be read");
    IpcError::from(ChainError::from(error))
}

#[cfg(test)]
mod tests {
    use jelly_net::UrlRejection;

    use super::*;

    #[test]
    fn a_rejected_url_is_invalid_input_and_a_broken_list_is_internal() {
        for reason in UrlRejection::ALL {
            let refused = url_failure(NetError::InvalidUrl { reason });
            assert_eq!(refused.code, ErrorCode::InvalidInput.into(), "{reason:?}");
            assert_eq!(refused.message, reason.message(), "{reason:?}");
        }

        let broken = url_failure(NetError::Defaults {
            chain: ChainId::Bitcoin,
            detail: "expected an array at line 3",
        });
        assert_eq!(broken.code, ErrorCode::Internal.into());
        assert!(!broken.message.contains("line 3"), "the parser's words reached the user");
    }
}
