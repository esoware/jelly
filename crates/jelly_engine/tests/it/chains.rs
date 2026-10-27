use std::collections::BTreeMap;
use std::time::Duration;

use jelly_core::{
    ChainId, CoreError, CursorBytes, CursorScope, KeyService, ProxySetting, ReservationSlot,
    SecretText, SkipReason,
};
use jelly_engine::{LockState, test_util};
use tokio::time::{pause, sleep};

use crate::common::{Fallible, Harness, PASSWORD, harness, secret};

const CHAIN: ChainId = ChainId::Bitcoin;

#[tokio::test]
async fn a_rebuild_publishes_one_generation() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();

    let builds = harness.builder.builds();
    test_util::rebuild_adapters(&harness.engine).await.unwrap();
    assert_eq!(harness.engine.sync_now(None).await.unwrap(), vec![CHAIN]);
    assert_eq!(harness.builder.builds(), builds.saturating_add(1));

    let request = harness.builder.last_request().expect("the fixture recorded a request");
    assert!(request.chains.contains(&CHAIN));
    assert!(request.account_key(ChainId::Monero).is_some(), "a seed wallet has a monero account");
}

#[tokio::test]
async fn a_chain_nothing_was_installed_for_has_no_adapter() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();

    let missing = test_util::adapter(&harness.engine, ChainId::Solana);
    assert!(
        matches!(missing, Err(CoreError::NoAdapter { chain: ChainId::Solana, reason: None })),
        "{missing:?}"
    );
}

#[tokio::test]
async fn only_the_chains_the_builder_built_are_synced() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN, ChainId::Solana, ChainId::Monero]);
    harness.builder.skip(ChainId::Solana, SkipReason::NoEndpoints);
    harness.builder.skip(ChainId::Monero, SkipReason::NoKeys);
    harness.seed_wallet("chains").await.unwrap();

    test_util::rebuild_adapters(&harness.engine).await.unwrap();
    assert_eq!(harness.engine.sync_now(None).await.unwrap(), vec![CHAIN]);

    let refused = harness.engine.sync_now(Some(ChainId::Solana)).await;
    assert!(
        matches!(
            refused,
            Err(CoreError::NoAdapter {
                chain: ChainId::Solana,
                reason: Some(SkipReason::NoEndpoints)
            })
        ),
        "{refused:?}"
    );
    let refused = harness.engine.sync_now(Some(ChainId::Monero)).await;
    assert!(
        matches!(
            refused,
            Err(CoreError::NoAdapter { chain: ChainId::Monero, reason: Some(SkipReason::NoKeys) })
        ),
        "{refused:?}"
    );
}

#[tokio::test]
async fn disabling_a_chain_takes_its_adapter_out_of_the_set() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();
    assert!(harness.engine.list_enabled_chains().await.unwrap().contains(&CHAIN));

    harness.engine.set_chain_enabled(CHAIN, false).await.unwrap();
    assert!(!harness.engine.list_enabled_chains().await.unwrap().contains(&CHAIN));

    let refused = harness.engine.sync_now(Some(CHAIN)).await;
    assert!(
        matches!(refused, Err(CoreError::NoAdapter { chain: CHAIN, reason: None })),
        "{refused:?}"
    );

    harness.engine.set_chain_enabled(CHAIN, true).await.unwrap();
    assert_eq!(harness.engine.sync_now(Some(CHAIN)).await.unwrap(), vec![CHAIN]);
}

#[tokio::test]
async fn a_disabled_chain_is_not_in_the_build_request() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();
    harness.engine.set_chain_enabled(CHAIN, false).await.unwrap();

    let request = harness.builder.last_request().expect("the fixture recorded a request");
    assert!(!request.chains.contains(&CHAIN));
}

#[tokio::test]
async fn locking_retires_the_generation() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();
    let weak = harness.builder.weak(CHAIN);
    assert!(weak.upgrade().is_some(), "the fixture never built an adapter");

    harness.engine.lock_wallet().await;
    assert!(weak.upgrade().is_none(), "a key-bearing adapter outlived its session");
    assert!(matches!(
        test_util::adapter(&harness.engine, CHAIN),
        Err(CoreError::NoAdapter { reason: None, .. })
    ));
}

#[tokio::test]
async fn a_rebuild_that_outlives_its_session_publishes_nothing() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();

    harness.builder.hold_next_build();
    let engine = harness.engine.clone();
    let rebuilding = tokio::spawn(async move { test_util::rebuild_adapters(&engine).await });
    harness.builder.build_started().await;
    harness.engine.lock_wallet().await;
    harness.builder.release();

    let outcome = rebuilding.await.unwrap();
    assert!(matches!(outcome, Err(CoreError::Locked)), "{outcome:?}");
    assert!(matches!(
        test_util::adapter(&harness.engine, CHAIN),
        Err(CoreError::NoAdapter { reason: None, .. })
    ));
    assert!(
        harness.builder.weak(CHAIN).upgrade().is_none(),
        "a key-bearing adapter outlived the session it was built for"
    );
}

#[tokio::test]
async fn a_rebuild_cannot_publish_into_a_reopened_session_of_the_same_wallet() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    let created = harness.seed_wallet("chains").await.unwrap();

    harness.builder.hold_next_build();
    let engine = harness.engine.clone();
    let rebuilding = tokio::spawn(async move { test_util::rebuild_adapters(&engine).await });
    harness.builder.build_started().await;
    harness.engine.lock_wallet().await;

    let mut states = harness.engine.lock_state();
    let engine = harness.engine.clone();
    let reopening = tokio::spawn(async move {
        engine.unlock_wallet(created.wallet.id, secret(PASSWORD), None).await
    });
    states.wait_for(LockState::is_unlocked).await.unwrap();
    harness.builder.release();

    assert!(matches!(rebuilding.await.unwrap(), Err(CoreError::Locked)));
    reopening.await.unwrap().unwrap();
    test_util::adapter(&harness.engine, CHAIN).unwrap();
}

#[tokio::test]
async fn a_poll_interval_change_keeps_the_adapters() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();
    let builds = harness.builder.builds();
    let weak = harness.builder.weak(CHAIN);

    harness.engine.set_poll_interval(CHAIN, Some(5)).await.unwrap();
    assert_eq!(harness.builder.builds(), builds, "the adapters were rebuilt");
    assert!(weak.upgrade().is_some(), "the adapter survived");
    assert_eq!(harness.engine.sync_now(Some(CHAIN)).await.unwrap(), vec![CHAIN]);

    harness.engine.set_poll_interval(CHAIN, None).await.unwrap();
    assert_eq!(harness.builder.builds(), builds);
}

#[tokio::test]
async fn a_poll_interval_of_zero_is_refused_as_invalid_input() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();

    let refused = harness.engine.set_poll_interval(CHAIN, Some(0)).await;
    assert!(
        matches!(refused, Err(CoreError::InvalidInput { field: "pollSecs", .. })),
        "{refused:?}"
    );
    harness.engine.set_poll_interval(CHAIN, Some(1)).await.unwrap();
}

/// The restore heights the wallet list shows for `wallet_id`.
async fn listed_restore_heights(
    harness: &Harness,
    wallet_id: i64,
) -> Fallible<BTreeMap<ChainId, u64>> {
    let wallets = harness.engine.list_wallets().await?;
    let wallet = wallets.into_iter().find(|wallet| wallet.id == wallet_id);
    Ok(wallet.ok_or("the wallet is not listed")?.restore_heights)
}

#[tokio::test]
async fn a_restore_height_is_remembered_and_rebuilds_the_adapters() {
    let harness = harness().await.unwrap();
    let created = harness.seed_wallet("monero").await.unwrap();
    assert!(created.wallet.restore_heights.is_empty());

    harness.engine.set_restore_height(ChainId::Monero, Some(3_000_000)).await.unwrap();
    let heights = listed_restore_heights(&harness, created.wallet.id).await.unwrap();
    assert_eq!(heights, BTreeMap::from([(ChainId::Monero, 3_000_000)]));
    let request = harness.builder.last_request().expect("the fixture recorded a request");
    assert_eq!(request.restore_height(ChainId::Monero), 3_000_000);

    harness.engine.set_restore_height(ChainId::Monero, None).await.unwrap();
    let heights = listed_restore_heights(&harness, created.wallet.id).await.unwrap();
    assert!(heights.is_empty());
    let request = harness.builder.last_request().expect("the fixture recorded a request");
    assert_eq!(request.restore_height(ChainId::Monero), 0);
}

#[tokio::test]
async fn a_rebuild_carries_the_stored_proxy_and_api_keys() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();
    let request = harness.builder.last_request().expect("the fixture recorded a request");
    assert_eq!(request.proxy, None);
    request.with_api_key(KeyService::Etherscan, |key| assert_eq!(key, None));
    request.with_api_key(KeyService::TronGrid, |key| assert_eq!(key, None));

    harness.engine.set_proxy(Some(ProxySetting::System)).await.unwrap();
    harness
        .engine
        .set_etherscan_api_key(Some(SecretText::new("etherscan-key".to_owned())))
        .await
        .unwrap();
    harness
        .engine
        .set_trongrid_api_key(Some(SecretText::new("trongrid-key".to_owned())))
        .await
        .unwrap();
    test_util::rebuild_adapters(&harness.engine).await.unwrap();

    let request = harness.builder.last_request().expect("the fixture recorded a request");
    assert_eq!(request.proxy, Some(ProxySetting::System));
    request.with_api_key(KeyService::Etherscan, |key| assert_eq!(key, Some("etherscan-key")));
    request.with_api_key(KeyService::TronGrid, |key| assert_eq!(key, Some("trongrid-key")));
}

#[tokio::test]
async fn endpoint_edits_are_debounced_into_one_rebuild() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();
    let builds = harness.builder.builds();
    pause();

    for _ in 0..5 {
        harness.engine.endpoints_changed();
    }
    harness.builder.builds_reach(builds.saturating_add(1)).await;
    // A rebuild of its own for any other edit would have woken with this one.
    sleep(Duration::from_millis(500)).await;
    assert_eq!(harness.builder.builds(), builds.saturating_add(1));
}

#[tokio::test]
async fn a_build_is_handed_a_cursor_store_that_reaches_the_vault() {
    let harness = harness().await.unwrap();
    harness.builder.install(&[CHAIN]);
    harness.seed_wallet("chains").await.unwrap();

    let cursors = harness.builder.cursors().expect("a request was recorded");
    // The wallet scope has no address row. Only the vault side of the port can answer
    // for it, so a write that succeeds proves the build got the routing port rather than
    // a database-only one.
    cursors
        .reserve(
            CursorScope::Wallet,
            ReservationSlot::MoneroSends,
            CursorBytes::copied(b"key images"),
        )
        .await
        .unwrap();
    let held =
        cursors.load_reservation(CursorScope::Wallet, ReservationSlot::MoneroSends).await.unwrap();
    assert!(held.is_some_and(|bytes| bytes.with_bytes(|read| read == b"key images")));
}
