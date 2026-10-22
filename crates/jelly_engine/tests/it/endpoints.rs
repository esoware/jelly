use jelly_core::{ChainId, CoreError, EndpointHealth, EndpointHealthRecord, EndpointOverride};

use crate::common::harness;

const CHAIN: ChainId = ChainId::Ethereum;
const URL: &str = "https://rpc.example.com";

fn health(url: &str, score_permille: u16) -> EndpointHealthRecord {
    EndpointHealthRecord {
        chain: CHAIN,
        url: url.to_owned(),
        health: EndpointHealth {
            score_permille,
            successes: 1,
            failures: 0,
            consecutive_failures: 0,
            latency_ms: Some(80),
            last_success_unix_secs: Some(1_700_000_000),
            last_failure_unix_secs: None,
            last_error: None,
        },
    }
}

#[tokio::test]
async fn endpoint_overrides_are_edited_with_no_wallet_open() {
    let harness = harness().await.unwrap();
    harness.engine.add_endpoint(CHAIN, URL.to_owned(), "my node".to_owned()).await.unwrap();
    harness.engine.set_endpoint_pinned(CHAIN, URL.to_owned(), true).await.unwrap();
    harness.engine.set_endpoint_enabled(CHAIN, URL.to_owned(), false).await.unwrap();

    assert_eq!(
        harness.engine.list_endpoints().await.unwrap(),
        vec![(
            CHAIN,
            EndpointOverride {
                url: URL.to_owned(),
                display: "my node".to_owned(),
                is_enabled: false,
                is_pinned: true,
            }
        )]
    );

    harness.engine.delete_endpoint(CHAIN, URL.to_owned()).await.unwrap();
    assert!(harness.engine.list_endpoints().await.unwrap().is_empty());
    let missing = harness.engine.delete_endpoint(CHAIN, URL.to_owned()).await;
    assert!(matches!(missing, Err(CoreError::NotFound { .. })), "{missing:?}");
}

#[tokio::test]
async fn endpoint_health_is_written_whole_or_not_at_all() {
    let harness = harness().await.unwrap();
    let refused = harness
        .engine
        .put_endpoint_health(vec![
            health("https://one.example", 900),
            health("https://two.example", 1_001),
        ])
        .await;
    assert!(matches!(refused, Err(CoreError::Storage { .. })), "{refused:?}");
    assert!(harness.engine.list_endpoint_health().await.unwrap().is_empty(), "half a write landed");

    let rows = vec![health("https://one.example", 900), health("https://two.example", 750)];
    harness.engine.put_endpoint_health(rows.clone()).await.unwrap();
    assert_eq!(harness.engine.list_endpoint_health().await.unwrap(), rows);

    harness.engine.put_endpoint_health(Vec::new()).await.unwrap();
    assert_eq!(harness.engine.list_endpoint_health().await.unwrap(), rows);
}
