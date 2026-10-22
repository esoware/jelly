//! Exercises real startup and shutdown with the price feed asking for nothing, to avoid
//! network access.

use std::fs;
use std::io::ErrorKind;
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;

use jelly_core::{ChainId, ErrorCode, PhraseWords, ProxySetting, WalletColor};
use jelly_engine::{PassphraseInput, StartupError};
use jelly_lib::{AppState, IpcError, Services, ShellConfig, StartupStatus};

use crate::common::{PASSWORD, deps, deps_without_keychain, offline_config, secret};

#[tokio::test]
async fn a_command_refuses_with_the_startup_failure_rather_than_a_reason_of_its_own() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::new(deps(directory.path()));
    assert_eq!(state.status(), StartupStatus::Starting);

    let outcome = Services::start(offline_config(), deps_without_keychain(directory.path())).await;
    assert!(outcome.is_err(), "a missing credential store is a startup failure");
    state.finish(outcome);

    let StartupStatus::Failed { error, can_repair_proxy, .. } = state.status() else {
        panic!("the status reports the failure");
    };
    assert_eq!(error.code, ErrorCode::Keychain.into());
    assert!(!can_repair_proxy);
    let unneeded = state.repair_proxy(None).await.expect_err("there is no failed proxy to repair");
    assert_eq!(unneeded.code, ErrorCode::InvalidInput.into());
    assert_eq!(unneeded.message, "Proxy repair is not needed.");

    let refused = state.engine().await.expect_err("nothing was built");
    assert_eq!(refused.code, ErrorCode::Keychain.into(), "a command answers the startup failure");
}

#[tokio::test]
async fn a_command_waits_for_startup_instead_of_refusing() {
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(AppState::new(deps(directory.path())));
    let waiting = tokio::spawn({
        let state = Arc::clone(&state);
        async move { state.engine().await.map(|_| ()) }
    });

    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    state.finish(Ok(Arc::clone(&services)));

    waiting.await.unwrap().expect("the wait should have succeeded");
    assert_eq!(state.status(), StartupStatus::Ready { rebuilt: None });

    services.shutdown().await;
}

#[tokio::test]
async fn a_replaced_database_reaches_the_startup_status() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("jelly.sqlite"), b"this is not a database").unwrap();

    let state = AppState::new(deps(directory.path()));
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    state.finish(Ok(Arc::clone(&services)));

    let StartupStatus::Ready { rebuilt: Some(rebuilt) } = state.status() else {
        panic!("the status carries what the rebuild cost");
    };
    assert!(
        Path::new(&rebuilt.moved_to).is_file(),
        "the unusable file is where the status says it went"
    );
    assert!(
        services.engine().unwrap().list_wallets().await.unwrap().is_empty(),
        "the new file is empty"
    );

    services.shutdown().await;
}

/// The start that replaces a damaged database then stops at the stored proxy.
///
/// The proxy lives in the settings file and survives the replacement. The user repairs it
/// and reopens Jelly, as the repair screen asks.
#[tokio::test]
async fn a_database_replaced_by_a_start_that_then_fails_is_still_reported() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let unusable = ProxySetting::Socks5h { url: String::from("nowhere at all") };
    services.engine().unwrap().set_proxy(Some(unusable)).await.unwrap();
    services.shutdown().await;
    fs::write(directory.path().join("jelly.sqlite"), b"this is not a database").unwrap();

    let state = AppState::new(deps(directory.path()));
    state.finish(Services::start(offline_config(), deps(directory.path())).await);
    let failed = state.status();
    assert!(
        matches!(failed, StartupStatus::Failed { can_repair_proxy: true, .. }),
        "the start stopped at the proxy: {failed:?}"
    );
    let moved_aside: Vec<String> = fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("jelly.damaged."))
        .collect();
    assert_eq!(moved_aside.len(), 1, "the damaged database was moved aside: {moved_aside:?}");
    state.repair_proxy(None).await.unwrap();

    let reopened = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let next = AppState::new(deps(directory.path()));
    next.finish(Ok(Arc::clone(&reopened)));
    let ready = next.status();
    reopened.shutdown().await;

    let is_told_when_failed = serde_json::to_string(&failed).unwrap().contains("movedTo");
    let is_told_when_reopened = matches!(ready, StartupStatus::Ready { rebuilt: Some(_) });
    assert!(
        is_told_when_failed || is_told_when_reopened,
        "the wallet database was replaced and moved to {moved_aside:?}, so its wallets are no \
         longer listed, yet neither the failed start ({failed:?}) nor the reopened one \
         ({ready:?}) tells the user"
    );
}

#[tokio::test]
async fn a_started_shell_lists_endpoints_for_every_chain_and_quotes_nothing_offline() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();

    for chain in ChainId::ALL {
        let rows = services.network().registry().configured(chain).unwrap();
        assert!(!rows.is_empty(), "{chain:?} has no endpoint to talk to");
    }
    assert!(services.prices().snapshot().is_empty());

    services.shutdown().await;
}

#[test]
fn the_shipped_price_feed_asks_for_at_least_one_ticker() {
    assert!(
        !ShellConfig::default().prices.tickers.is_empty(),
        "the shipped feed must quote something"
    );
}

/// The price feed runs while locked. It must follow proxy changes without waiting for an
/// adapter rebuild.
#[tokio::test]
async fn a_new_route_reaches_the_shared_client_with_no_wallet_open() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    assert_eq!(services.network().proxy(), None, "the process starts direct");

    services.set_proxy(Some(ProxySetting::System)).await.unwrap();
    assert_eq!(services.network().proxy(), Some(ProxySetting::System));
    assert_eq!(
        services.engine().unwrap().app_settings().await.proxy,
        Some(ProxySetting::System),
        "the route the client took is the one on disk"
    );

    services.shutdown().await;
}

/// Reject invalid proxies before saving them, or the next startup cannot reach settings.
#[tokio::test]
async fn a_route_the_client_refuses_is_never_stored() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();

    let unusable = ProxySetting::Socks5h { url: String::from("nowhere at all") };
    let refused = services.set_proxy(Some(unusable)).await.expect_err("the client cannot be built");
    assert_eq!(refused.code, ErrorCode::InvalidInput.into());
    assert_eq!(services.engine().unwrap().app_settings().await.proxy, None, "nothing was stored");
    assert_eq!(services.network().proxy(), None, "the process is still direct");

    services.shutdown().await;
}

/// A second shell keeps the file open throughout.
///
/// `SQLite` checkpoints by itself when the last connection closes, which would hide an
/// engine that was dropped rather than shut down. With another connection open, only an
/// explicit checkpoint empties the log.
#[tokio::test]
async fn a_startup_failure_after_the_engine_opened_still_checkpoints_the_database() {
    let directory = tempfile::tempdir().unwrap();
    let running = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let unusable = ProxySetting::Socks5h { url: String::from("nowhere at all") };
    running.engine().unwrap().set_proxy(Some(unusable)).await.unwrap();
    let log = directory.path().join("jelly.sqlite-wal");
    assert!(fs::metadata(&log).unwrap().len() > 0, "the first start's writes are in the log");

    let failed = Services::start(offline_config(), deps(directory.path()))
        .await
        .expect_err("the stored route is refused after the engine opened");
    assert!(matches!(failed.error, StartupError::NetworkSettings { .. }), "{failed:?}");
    assert_eq!(
        fs::metadata(&log).unwrap().len(),
        0,
        "the failed start left the log for the next open to replay"
    );

    running.shutdown().await;
}

/// An open wallet starts syncing at once, so its traffic goes to a local proxy that never
/// answers rather than to the shipped nodes.
#[tokio::test]
async fn shutting_down_takes_the_engine_and_checkpoints_the_database() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let silent_proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("socks5h://{}", silent_proxy.local_addr().unwrap());
    services.set_proxy(Some(ProxySetting::Socks5h { url })).await.unwrap();
    let engine = services.engine().unwrap();

    let created = engine
        .create_seed_wallet(
            "first",
            WalletColor::default(),
            secret(PASSWORD),
            PhraseWords::Twelve,
            PassphraseInput::None,
        )
        .await
        .unwrap();
    engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await.unwrap();
    assert!(engine.lock_state_now().is_unlocked());
    drop(engine);

    services.shutdown().await;

    let refused = services.engine().expect_err("the handle was taken at shutdown");
    assert_eq!(refused.code, ErrorCode::Internal.into());
    // The network and the factory outlive the engine, and still the database closed.
    let log = directory.path().join("jelly.sqlite-wal");
    assert_eq!(fs::metadata(log).map_or(0, |found| found.len()), 0);
}

#[tokio::test]
async fn a_stored_route_the_client_refuses_is_not_blamed_on_the_database() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let unusable = ProxySetting::Socks5h { url: String::from("nowhere at all") };
    services.engine().unwrap().set_proxy(Some(unusable)).await.unwrap();
    services.shutdown().await;

    let failure = Services::start(offline_config(), deps(directory.path()))
        .await
        .expect_err("the stored route is refused after the database opened");
    let shown = IpcError::from(failure.error);
    assert_eq!(
        shown.code,
        ErrorCode::InvalidInput.into(),
        "the database opened and read fine, so the startup screen blames the stored route"
    );
}

#[tokio::test]
async fn a_failed_proxy_save_leaves_live_and_saved_routes_unchanged() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    services.set_proxy(Some(ProxySetting::System)).await.unwrap();
    let path = directory.path().join("settings.json");
    let saved = directory.path().join("settings.saved");
    let before = fs::read(&path).unwrap();
    fs::rename(&path, &saved).unwrap();
    fs::create_dir(&path).unwrap();
    let refused = services.set_proxy(None).await.expect_err("the settings file cannot be written");
    assert_eq!(refused.code, ErrorCode::Storage.into());
    assert_eq!(services.network().proxy(), Some(ProxySetting::System));
    assert_eq!(services.engine().unwrap().app_settings().await.proxy, Some(ProxySetting::System));
    assert_eq!(fs::read(&saved).unwrap(), before);
    fs::remove_dir(&path).unwrap();
    fs::rename(&saved, &path).unwrap();
    services.set_proxy(None).await.unwrap();
    assert_eq!(services.network().proxy(), None);
    assert_eq!(services.engine().unwrap().app_settings().await.proxy, None);
    services.shutdown().await;
}

#[tokio::test]
async fn concurrent_proxy_changes_publish_the_last_persisted_route() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("socks5h://{}", listener.local_addr().unwrap());
    let setting = ProxySetting::Socks5h { url: url.clone() };
    let (first, second) = tokio::join!(
        services.set_proxy(Some(ProxySetting::System)),
        services.set_proxy(Some(setting.clone())),
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(services.network().proxy(), Some(ProxySetting::Socks5h { url }));
    assert_eq!(services.engine().unwrap().app_settings().await.proxy, Some(setting));
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    services.shutdown().await;
}

#[tokio::test]
async fn proxy_repair_saves_offline_and_preserves_unrelated_settings() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    let engine = services.engine().unwrap();
    engine.set_auto_lock_minutes(7).await.unwrap();
    engine.set_proxy(Some(ProxySetting::Socks5h { url: "not a proxy".to_owned() })).await.unwrap();
    let before = engine.app_settings().await;
    drop(engine);
    services.shutdown().await;

    let state = AppState::new(deps(directory.path()));
    state.finish(Services::start(offline_config(), deps(directory.path())).await);
    assert!(matches!(state.status(), StartupStatus::Failed { can_repair_proxy: true, .. }));
    let saved_path = directory.path().join("settings.json");
    let saved = fs::read(&saved_path).unwrap();
    let refused = state
        .repair_proxy(Some(ProxySetting::Socks5h { url: String::new() }))
        .await
        .expect_err("an empty proxy address cannot be used");
    assert_eq!(refused.code, ErrorCode::InvalidInput.into());
    assert_eq!(fs::read(&saved_path).unwrap(), saved);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let proxy =
        ProxySetting::Socks5h { url: format!("socks5h://{}", listener.local_addr().unwrap()) };
    let (first, second) = tokio::join!(
        state.repair_proxy(Some(ProxySetting::System)),
        state.repair_proxy(Some(proxy.clone())),
    );
    first.unwrap();
    second.unwrap();
    state.repair_proxy(Some(proxy.clone())).await.unwrap();
    let refused = state.engine().await.expect_err("repair does not make services available");
    assert_eq!(
        refused.code,
        ErrorCode::InvalidInput.into(),
        "commands still answer the startup failure"
    );
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    let stored: jelly_engine::AppSettings =
        serde_json::from_slice(&fs::read(&saved_path).unwrap()).unwrap();
    assert_eq!(stored, jelly_engine::AppSettings { proxy: Some(proxy), ..before });
    let restarted = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    assert_eq!(restarted.engine().unwrap().app_settings().await, stored);
    restarted.shutdown().await;
}

#[tokio::test]
async fn proxy_repair_is_unavailable_while_starting_or_ready() {
    let directory = tempfile::tempdir().unwrap();
    let state = AppState::new(deps(directory.path()));
    let starting = state.repair_proxy(None).await.expect_err("nothing has failed while starting");
    assert_eq!(starting.code, ErrorCode::InvalidInput.into());
    assert_eq!(starting.message, "Proxy repair is not needed.");
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    state.finish(Ok(Arc::clone(&services)));
    let ready = state
        .repair_proxy(Some(ProxySetting::System))
        .await
        .expect_err("nothing has failed once ready");
    assert_eq!(ready.code, ErrorCode::InvalidInput.into());
    assert_eq!(ready.message, "Proxy repair is not needed.");
    assert_eq!(services.network().proxy(), None);
    services.shutdown().await;
}

#[tokio::test]
async fn shutting_down_twice_is_harmless() {
    let directory = tempfile::tempdir().unwrap();
    let services = Services::start(offline_config(), deps(directory.path())).await.unwrap();
    services.shutdown().await;
    services.shutdown().await;
}
