use jelly_core::{ChainId, EndpointHealth, EndpointHealthRecord, EndpointOverride};
use jelly_store::{Store, StoreError};

use crate::common::{self, Fallible};

async fn add(store: &Store, url: &str) -> Result<(), StoreError> {
    store.add_endpoint(ChainId::Bitcoin, url.to_owned(), url.to_owned()).await
}

/// The one row the store lists.
async fn only_endpoint(store: &Store) -> Fallible<(ChainId, EndpointOverride)> {
    let rows = store.list_endpoints().await?;
    let [row] = <[(ChainId, EndpointOverride); 1]>::try_from(rows)
        .map_err(|rows| format!("expected one endpoint, got {rows:?}"))?;
    Ok(row)
}

async fn put_health(store: &Store, record: EndpointHealthRecord) -> Result<(), StoreError> {
    store.transact(move |writer| writer.put_endpoint_health(&record)).await
}

fn health(url: &str, score: u16) -> EndpointHealthRecord {
    EndpointHealthRecord {
        chain: ChainId::Ethereum,
        url: url.to_owned(),
        health: EndpointHealth {
            score_permille: score,
            successes: 12,
            failures: 3,
            consecutive_failures: 1,
            latency_ms: Some(240),
            last_success_unix_secs: Some(1_700_000_000),
            last_failure_unix_secs: None,
            last_error: Some(String::from("timeout")),
        },
    }
}

#[tokio::test]
async fn the_url_is_stored_and_returned_verbatim() {
    let harness = common::harness().await.unwrap();
    let urls = [
        "https://node.example.com/rpc?key=one",
        "https://node.example.com/rpc?key=two",
        "ssl://node.example.com:50002",
    ];
    for url in urls {
        add(&harness.store, url).await.unwrap();
    }

    let rows = harness.store.list_endpoints().await.unwrap();
    assert_eq!(rows.iter().map(|(_, row)| row.url.as_str()).collect::<Vec<_>>(), urls);
    assert!(rows.iter().all(|(_, row)| row.display == row.url), "{rows:?}");
}

#[tokio::test]
async fn a_display_name_can_differ_from_the_key() {
    let harness = common::harness().await.unwrap();
    harness
        .store
        .add_endpoint(
            ChainId::Bitcoin,
            String::from("https://node.example.com"),
            String::from("my node"),
        )
        .await
        .unwrap();
    let (_, row) = only_endpoint(&harness.store).await.unwrap();
    assert_eq!(row.url, "https://node.example.com");
    assert_eq!(row.display, "my node");
}

#[tokio::test]
async fn setting_a_flag_creates_the_row_when_it_is_not_there_yet() {
    let harness = common::harness().await.unwrap();
    let url = String::from("https://fresh.example.com");

    harness.store.set_endpoint_enabled(ChainId::Bitcoin, url.clone(), false).await.unwrap();
    let (_, row) = only_endpoint(&harness.store).await.unwrap();
    assert_eq!(row.url, url);
    assert_eq!(row.display, url);
    assert!(!row.is_enabled);
    assert!(!row.is_pinned);

    harness.store.set_endpoint_pinned(ChainId::Bitcoin, url, true).await.unwrap();
    let (_, row) = only_endpoint(&harness.store).await.unwrap();
    assert!(row.is_pinned);
    assert!(!row.is_enabled, "pinning must not re-enable a disabled endpoint");
}

#[tokio::test]
async fn adding_a_listed_endpoint_re_enables_it_and_keeps_its_pin() {
    let harness = common::harness().await.unwrap();
    let url = String::from("https://a.example.com");
    add(&harness.store, &url).await.unwrap();
    harness.store.set_endpoint_pinned(ChainId::Bitcoin, url.clone(), true).await.unwrap();
    harness.store.set_endpoint_enabled(ChainId::Bitcoin, url.clone(), false).await.unwrap();

    harness
        .store
        .add_endpoint(ChainId::Bitcoin, url.clone(), String::from("renamed"))
        .await
        .unwrap();
    let (_, again) = only_endpoint(&harness.store).await.unwrap();
    assert_eq!(again.display, "renamed");
    assert!(again.is_enabled, "adding an endpoint again is how a disabled one comes back");
    assert!(again.is_pinned, "adding an endpoint again must not unpin it");

    // The key is the pair, so the same endpoint under another chain is its own row.
    harness.store.add_endpoint(ChainId::Solana, url.clone(), url).await.unwrap();
    assert_eq!(harness.store.list_endpoints().await.unwrap().len(), 2);
}

#[tokio::test]
async fn deleting_an_absent_endpoint_reports_it_as_missing() {
    let harness = common::harness().await.unwrap();
    let url = String::from("https://gone.example.com");
    add(&harness.store, &url).await.unwrap();
    harness.store.delete_endpoint(ChainId::Bitcoin, url.clone()).await.unwrap();

    let error =
        harness.store.delete_endpoint(ChainId::Bitcoin, url).await.expect_err("the second attempt");
    assert!(matches!(error, StoreError::NotFound { entity: "endpoint", .. }), "{error:?}");
}

#[tokio::test]
async fn a_missing_endpoint_is_named_without_its_path_or_query() {
    let harness = common::harness().await.unwrap();
    let url = String::from("https://rpc.example.com/v3/SUPER-SECRET-KEY?token=ALSO-SECRET");

    let error =
        harness.store.delete_endpoint(ChainId::Ethereum, url).await.expect_err("no such row");
    let message = error.to_string();
    assert!(!message.contains("SUPER-SECRET-KEY"), "{message}");
    assert!(!message.contains("ALSO-SECRET"), "{message}");
    assert!(message.contains("https://rpc.example.com"), "{message}");
}

#[tokio::test]
async fn debug_never_prints_an_endpoint_url_that_carries_a_key() {
    let harness = common::harness().await.unwrap();
    let url = "https://rpc.example.com/v3/ENDPOINTSECRET?token=QUERYSECRET";
    harness.store.set_endpoint_enabled(ChainId::Ethereum, url.to_owned(), false).await.unwrap();
    let rendered = format!("{:?}", only_endpoint(&harness.store).await.unwrap());
    assert!(!rendered.contains("ENDPOINTSECRET"), "{rendered}");
    assert!(!rendered.contains("QUERYSECRET"), "{rendered}");

    let rendered = format!("{:?}", health(url, 900));
    assert!(!rendered.contains("ENDPOINTSECRET"), "{rendered}");
    assert!(!rendered.contains("QUERYSECRET"), "{rendered}");
}

#[tokio::test]
async fn health_survives_a_round_trip_and_is_keyed_by_chain_and_url() {
    let harness = common::harness().await.unwrap();
    put_health(&harness.store, health("https://rpc.example.com", 900)).await.unwrap();
    put_health(&harness.store, health("https://rpc.example.com", 750)).await.unwrap();
    let elsewhere =
        EndpointHealthRecord { chain: ChainId::Bitcoin, ..health("https://rpc.example.com", 500) };
    put_health(&harness.store, elsewhere).await.unwrap();

    let rows = harness.store.list_endpoint_health().await.unwrap();
    assert_eq!(rows.len(), 2, "the same url under another chain is a row of its own");
    let bitcoin = rows.iter().find(|row| row.chain == ChainId::Bitcoin).expect("bitcoin");
    assert_eq!(bitcoin.health.score_permille, 500);
    let row = rows.iter().find(|row| row.chain == ChainId::Ethereum).expect("ethereum");
    assert_eq!(row.url, "https://rpc.example.com");
    let health = &row.health;
    assert_eq!(health.score_permille, 750);
    assert_eq!(health.successes, 12);
    assert_eq!(health.failures, 3);
    assert_eq!(health.consecutive_failures, 1);
    assert_eq!(health.latency_ms, Some(240));
    assert_eq!(health.last_success_unix_secs, Some(1_700_000_000));
    assert_eq!(health.last_failure_unix_secs, None);
    assert_eq!(health.last_error.as_deref(), Some("timeout"));
}

#[tokio::test]
async fn a_score_the_schema_does_not_allow_is_refused() {
    let harness = common::harness().await.unwrap();
    let error = put_health(&harness.store, health("https://rpc.example.com", 1_001))
        .await
        .expect_err("out of range");
    assert!(matches!(error, StoreError::Rejected { entity: "endpoint health", .. }), "{error:?}");
}
