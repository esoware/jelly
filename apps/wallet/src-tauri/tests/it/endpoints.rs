//! The shell's bridge between the engine's endpoint tables and the network layer, driven
//! through real startup, reloads, and shutdown with the price feed asking for nothing.

use std::path::Path;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

use jelly_core::{ChainId, Transport, test_util::FakeClock};
use jelly_lib::{Services, ShellDeps};
use jelly_net::{Endpoint, EndpointOrigin, HealthClass, NetError};
use jelly_vault::test_util::MemoryKeyStore;
use tokio::sync::Notify;
use tokio::time::timeout;

use crate::common::{Fallible, deps, offline_config};

const CHAIN: ChainId = ChainId::Bitcoin;
const TRANSPORT: Transport = Transport::ElectrumSsl;
/// Already canonical, because the endpoint commands canonicalize a URL before it reaches
/// a stored row.
const MY_NODE_URL: &str = "ssl://node.example:50002";
const START_UNIX_MILLIS: u64 = 1_767_225_600_000;

fn deps_on(data_dir: &Path, clock: &Arc<FakeClock>) -> ShellDeps {
    ShellDeps {
        data_dir: data_dir.to_path_buf(),
        clock: Arc::<FakeClock>::clone(clock),
        keystore: Arc::new(MemoryKeyStore::new()),
    }
}

fn manual_clock() -> Arc<FakeClock> {
    Arc::new(FakeClock::new(START_UNIX_MILLIS))
}

/// The URLs a pool on `chain` and `transport` walks, in configured order.
fn rotation(services: &Services, chain: ChainId, transport: Transport) -> Fallible<Vec<String>> {
    Ok(services
        .network()
        .registry()
        .enabled(chain, transport)?
        .iter()
        .map(|endpoint| endpoint.url().to_owned())
        .collect())
}

#[tokio::test]
async fn a_reload_replaces_the_whole_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let engine = services.engine().unwrap();
    let shipped_urls = rotation(&services, CHAIN, TRANSPORT).unwrap();
    let curated = shipped_urls.first().cloned().unwrap();

    engine.add_endpoint(CHAIN, MY_NODE_URL.to_owned(), "my node".to_owned()).await.unwrap();
    engine.set_endpoint_enabled(CHAIN, curated.clone(), false).await.unwrap();
    services.endpoints_changed().await.unwrap();

    let edited_urls = rotation(&services, CHAIN, TRANSPORT).unwrap();
    assert!(edited_urls.iter().any(|url| url == MY_NODE_URL), "{edited_urls:?}");
    assert!(!edited_urls.contains(&curated), "{edited_urls:?}");

    engine.delete_endpoint(CHAIN, MY_NODE_URL.to_owned()).await.unwrap();
    engine.delete_endpoint(CHAIN, curated).await.unwrap();
    services.endpoints_changed().await.unwrap();
    assert_eq!(rotation(&services, CHAIN, TRANSPORT).unwrap(), shipped_urls);

    drop(engine);
    services.shutdown().await;
}

#[tokio::test]
async fn an_override_carries_its_whole_row_into_the_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let engine = services.engine().unwrap();
    engine.add_endpoint(CHAIN, MY_NODE_URL.to_owned(), "my node".to_owned()).await.unwrap();
    engine.set_endpoint_pinned(CHAIN, MY_NODE_URL.to_owned(), true).await.unwrap();
    services.endpoints_changed().await.unwrap();

    let configured = services.network().registry().configured(CHAIN).unwrap();
    let mine = configured
        .iter()
        .find(|status| status.endpoint.url() == MY_NODE_URL)
        .expect("the stored row did not reach the registry");
    assert_eq!(mine.endpoint.input(), "my node");
    assert_eq!(mine.endpoint.origin(), EndpointOrigin::Pinned);
    assert!(mine.is_enabled);
    assert_eq!(
        rotation(&services, CHAIN, TRANSPORT).unwrap().first().map(String::as_str),
        Some(MY_NODE_URL)
    );

    drop(engine);
    services.shutdown().await;
}

#[tokio::test]
async fn a_stored_plaintext_override_joins_the_rotation() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let plaintext = "http://198.51.100.7:8545";
    services
        .engine()
        .unwrap()
        .add_endpoint(ChainId::Solana, plaintext.to_owned(), "my node".to_owned())
        .await
        .unwrap();
    services.endpoints_changed().await.unwrap();

    let urls = rotation(&services, ChainId::Solana, Transport::HttpJsonrpc).unwrap();
    assert!(urls.iter().any(|url| url == plaintext), "{urls:?}");

    services.shutdown().await;
}

#[tokio::test]
async fn a_pinned_override_reaches_the_registry_at_the_head_of_the_list() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let engine = services.engine().unwrap();

    let url = String::from("https://rpc.example.test");
    engine.add_endpoint(ChainId::Ethereum, url.clone(), url.clone()).await.unwrap();
    engine.set_endpoint_pinned(ChainId::Ethereum, url, true).await.unwrap();
    services.endpoints_changed().await.unwrap();

    let rows = services.network().registry().configured(ChainId::Ethereum).unwrap();
    let added = rows
        .iter()
        .find(|status| status.endpoint.url() == "https://rpc.example.test")
        .expect("the override reached the registry");
    assert_eq!(added.endpoint.origin(), EndpointOrigin::Pinned);
    // The list puts pinned endpoints first. Pools still rank by health, so pinning only
    // breaks ties there.
    assert_eq!(rows.first().map(|status| status.endpoint.url()), Some("https://rpc.example.test"));

    services.shutdown().await;
}

/// Wakes the test when the reload it was handed to has finished reading the table.
struct ReadFinished(Notify);

impl Wake for ReadFinished {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }
}

/// Two endpoint commands from the settings page run at once.
///
/// Each is a write followed by a reload. The first reload reads the table before the
/// second command's write and is resumed only after the second command has finished, as a
/// busy runtime may resume it.
#[tokio::test]
async fn a_reload_that_read_before_a_concurrent_edit_does_not_revert_it() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let engine = services.engine().unwrap();
    let registry = services.network().registry();
    let shipped = registry.configured(ChainId::Ethereum).unwrap()[0].endpoint.url().to_owned();
    let added = String::from("https://rpc.example.test");

    engine.add_endpoint(ChainId::Ethereum, added.clone(), added).await.unwrap();
    {
        let read_finished = Arc::new(ReadFinished(Notify::new()));
        let waker = Waker::from(Arc::clone(&read_finished));
        // On a busy machine the read can finish before the first poll checks on it, and the
        // reload then completes without pausing, so reloads start until one pauses.
        let mut paused_reload = None;
        for _ in 0..100 {
            let mut reload = Box::pin(services.endpoints_changed());
            match reload.as_mut().poll(&mut Context::from_waker(&waker)) {
                Poll::Pending => {
                    paused_reload = Some(reload);
                    break;
                }
                Poll::Ready(result) => result.unwrap(),
            }
        }
        let mut first_reload = paused_reload.expect("no reload paused to read the table");
        read_finished.0.notified().await;

        let mut second_command = pin!(async {
            engine.set_endpoint_enabled(ChainId::Ethereum, shipped.clone(), false).await.unwrap();
            services.endpoints_changed().await.unwrap();
        });
        // Reloads that wait for one another cannot let the second finish first, so after a
        // while the first is resumed and both run to the end.
        if timeout(Duration::from_secs(10), second_command.as_mut()).await.is_err() {
            let (first, ()) = tokio::join!(first_reload.as_mut(), second_command.as_mut());
            first.unwrap();
        } else {
            first_reload.await.unwrap();
        }
    }

    let stored = engine.list_endpoints().await.unwrap();
    let is_disabled_on_disk = stored.iter().any(|(_, row)| row.url == shipped && !row.is_enabled);
    let live = registry.configured(ChainId::Ethereum).unwrap();
    let is_enabled_live =
        live.iter().any(|status| status.endpoint.url() == shipped && status.is_enabled);
    drop(engine);
    services.shutdown().await;

    assert!(is_disabled_on_disk, "the database records that the user disabled {shipped}");
    assert!(
        !is_enabled_live,
        "the user disabled {shipped} and the database says so, yet the registry that the pools \
         and the endpoint list read still has it enabled"
    );
}

/// No pool reads such a row and the endpoint list cannot show it for the user to
/// delete.
#[tokio::test]
async fn an_unreadable_endpoint_row_is_deleted_and_its_chain_keeps_the_curated_list() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let engine = services.engine().unwrap();
    engine
        .add_endpoint(ChainId::Solana, "not an address".to_owned(), "my node".to_owned())
        .await
        .unwrap();
    services.endpoints_changed().await.unwrap();

    assert!(engine.list_endpoints().await.unwrap().is_empty());
    assert!(!services.network().registry().configured(ChainId::Solana).unwrap().is_empty());

    drop(engine);
    services.shutdown().await;
}

/// A stored row on a transport its chain never uses still lets the shell start. It is
/// deleted at the first start and never listed.
#[tokio::test]
async fn a_stored_endpoint_on_a_transport_its_chain_never_uses_is_deleted_at_startup() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let url = String::from("wss://socket.example.test");
    services
        .engine()
        .unwrap()
        .add_endpoint(ChainId::Bitcoin, url.clone(), url.clone())
        .await
        .unwrap();
    services.shutdown().await;

    for _ in 0..2 {
        let restarted = Services::start(offline_config(), deps(directory.path())).await.unwrap();
        let stored = restarted.engine().unwrap().list_endpoints().await.unwrap();
        assert!(stored.iter().all(|(_, row)| row.url != url), "the row was deleted");
        let rows = restarted.network().registry().configured(ChainId::Bitcoin).unwrap();
        assert!(rows.iter().all(|status| status.endpoint.url() != url), "and is not listed");
        assert!(!rows.is_empty(), "the shipped endpoints are still there");
        restarted.shutdown().await;
    }
}

#[tokio::test]
async fn health_crosses_a_restart_and_a_quarantine_does_not() {
    let directory = tempfile::tempdir().unwrap();
    let clock = manual_clock();
    let endpoint = Endpoint::parse(CHAIN, TRANSPORT, MY_NODE_URL).unwrap();
    let failure = NetError::timed_out(endpoint.label(), Duration::from_secs(1));

    let first = Services::start(offline_config(), deps_on(directory.path(), &clock)).await.unwrap();
    let health = first.network().health();
    health.record_success(&endpoint, Duration::from_millis(120));
    for _ in 0..3 {
        health.record_failure(&endpoint, &failure);
    }
    assert_eq!(health.entry(CHAIN, MY_NODE_URL).class, HealthClass::Quarantined);
    first.shutdown().await;

    let second =
        Services::start(offline_config(), deps_on(directory.path(), &clock)).await.unwrap();
    let written_records = second.engine().unwrap().list_endpoint_health().await.unwrap();
    let stored = written_records.first().expect("nothing was written");
    assert_eq!(stored.chain, CHAIN);
    assert_eq!(stored.url, MY_NODE_URL);
    assert_eq!(stored.health.successes, 1);
    assert_eq!(stored.health.failures, 3);
    assert_eq!(stored.health.consecutive_failures, 3);
    assert_eq!(stored.health.latency_ms, Some(120));
    assert_eq!(stored.health.last_error, Some(failure.to_string()));

    let entry = second.network().health().entry(CHAIN, MY_NODE_URL);
    assert_eq!(entry.health.score_permille, stored.health.score_permille);
    assert_eq!(entry.health.successes, stored.health.successes);
    assert_eq!(entry.health.failures, stored.health.failures);
    assert_eq!(entry.health.consecutive_failures, stored.health.consecutive_failures);
    assert_eq!(entry.health.latency_ms, stored.health.latency_ms);
    assert_eq!(entry.health.last_error, stored.health.last_error);
    assert_ne!(entry.class, HealthClass::Quarantined, "a quarantine is a judgement about one run");
    second.shutdown().await;

    // The restored tracker writes back exactly the row it was built from.
    let third = Services::start(offline_config(), deps_on(directory.path(), &clock)).await.unwrap();
    assert_eq!(third.engine().unwrap().list_endpoint_health().await.unwrap(), written_records);
    third.shutdown().await;
}

#[tokio::test]
async fn a_session_that_touched_no_endpoint_writes_no_row() {
    let directory = tempfile::tempdir().unwrap();
    let first = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    first.shutdown().await;

    let second = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    assert!(second.engine().unwrap().list_endpoint_health().await.unwrap().is_empty());
    second.shutdown().await;
}

/// Restoring seeds only stored rows, never a record for each curated endpoint.
#[tokio::test]
async fn an_empty_table_restores_no_record() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    assert!(services.network().health().snapshot(CHAIN).is_empty());
    services.shutdown().await;
}

/// A restored score is aged by how long ago the endpoint was last heard from.
///
/// A run that sends the endpoint nothing learns nothing new, so the run after it must
/// restore the same score rather than age the already aged one again from the same moment.
#[tokio::test]
async fn a_restart_that_touches_no_endpoint_does_not_age_its_health_again() {
    let directory = tempfile::tempdir().unwrap();
    let clock = manual_clock();
    let endpoint = Endpoint::parse(CHAIN, TRANSPORT, MY_NODE_URL).unwrap();
    let failure = NetError::timed_out(endpoint.label(), Duration::from_secs(1));

    let first_run =
        Services::start(offline_config(), deps_on(directory.path(), &clock)).await.unwrap();
    first_run.network().health().record_failure(&endpoint, &failure);
    first_run.network().health().record_failure(&endpoint, &failure);
    first_run.shutdown().await;

    clock.advance(Duration::from_secs(3600));
    let second_run =
        Services::start(offline_config(), deps_on(directory.path(), &clock)).await.unwrap();
    let restored = second_run.network().health().entry(CHAIN, MY_NODE_URL);
    second_run.shutdown().await;

    let third_run =
        Services::start(offline_config(), deps_on(directory.path(), &clock)).await.unwrap();
    let again = third_run.network().health().entry(CHAIN, MY_NODE_URL);
    third_run.shutdown().await;
    assert_eq!(
        (again.health.score_permille, again.class),
        (restored.health.score_permille, restored.class),
        "a run that sent the endpoint nothing should hand on a score of {} ({:?}), but the \
         next run, at the same moment, restored {} ({:?})",
        restored.health.score_permille,
        restored.class,
        again.health.score_permille,
        again.class
    );
}
