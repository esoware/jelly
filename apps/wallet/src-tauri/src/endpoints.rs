use jelly_core::{ChainId, CoreError, EndpointHealthRecord};
use jelly_engine::WalletEngine;
use jelly_net::{EndpointRegistry, HealthEntry, HealthTracker};

const TARGET: &str = "jelly::app::endpoints";

/// Replaces every chain's overrides with the stored rows, so an endpoint the user deleted
/// leaves the rotation in the same step that a new one joins it.
///
/// Deletes a stored row the registry can't parse, since no pool reads it and the endpoint
/// list can't show it for the user to delete.
pub(crate) async fn load_endpoint_overrides(
    engine: &WalletEngine,
    registry: &EndpointRegistry,
) -> Result<(), CoreError> {
    let mut overrides = Vec::new();
    for (chain, endpoint) in engine.list_endpoints().await? {
        let Err(validation) = registry.parse_stored(chain, &endpoint.url) else {
            overrides.push((chain, endpoint));
            continue;
        };
        let transport = jelly_chains::chains().spec(chain).transport_for(&endpoint.url);
        match engine.delete_endpoint(chain, endpoint.url).await {
            Ok(()) => tracing::warn!(
                target: TARGET,
                chain = chain.as_str(),
                transport = transport.as_str(),
                %validation,
                "an unusable stored endpoint was deleted"
            ),
            Err(error) => tracing::warn!(
                target: TARGET,
                chain = chain.as_str(),
                transport = transport.as_str(),
                %validation,
                %error,
                "an unusable stored endpoint couldn't be deleted"
            ),
        }
    }
    registry.load_overrides(overrides);
    Ok(())
}

/// Restores stored endpoint health, so the first rotation starts with the endpoint that
/// last worked.
///
/// The tracker ages each record toward the baseline, and quarantines are never stored, so
/// last week's outage doesn't decide today's order.
pub(crate) async fn restore_endpoint_health(
    engine: &WalletEngine,
    tracker: &HealthTracker,
) -> Result<(), CoreError> {
    for EndpointHealthRecord { chain, url, health } in engine.list_endpoint_health().await? {
        tracker.restore(chain, url, health);
    }
    Ok(())
}

/// Writes the records this run touched, in one transaction.
///
/// A record that was only restored keeps its stored row, so every later restore ages the
/// same score from the same last-seen time.
pub(crate) async fn persist_endpoint_health(
    engine: &WalletEngine,
    tracker: &HealthTracker,
) -> Result<(), CoreError> {
    let records: Vec<EndpointHealthRecord> = ChainId::ALL
        .into_iter()
        .flat_map(|chain| {
            let touched = tracker.snapshot(chain).into_iter().filter(|entry| entry.is_touched);
            touched.map(move |HealthEntry { url, health, .. }| EndpointHealthRecord {
                chain,
                url,
                health,
            })
        })
        .collect();
    engine.put_endpoint_health(records).await
}
