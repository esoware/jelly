use std::time::Duration;

use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainId, ChainSyncProgress, Clock as _, TxConfirmation,
    TxDirection, TxId, TxOutcome, TxRecord, WalletEvent,
};
use jelly_engine::{EngineConfig, TxQuery, test_util};
use tokio::sync::broadcast;

use crate::common::{self, Fallible, harness, harness_with, record};

const CHAIN: ChainId = ChainId::Bitcoin;

const fn asset() -> AssetId {
    AssetId::Native(CHAIN)
}

#[tokio::test]
async fn balance_updates_coalesce_to_the_latest_in_the_window() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("coalescing").await.unwrap();
    let added = harness.engine.add_address(CHAIN, None, None).await.unwrap();
    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);

    for units in 1u128..=3 {
        sink.emit(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: added.id,
            asset: asset(),
            balance: Amount::from_base_units(units),
        }))
        .await;
    }
    sink.emit(WalletEvent::TxDetected(record(added.id, "coalescing-marker"))).await;

    // The lossless marker leaves the pump first, then the window's survivors.
    assert!(matches!(events.recv().await.unwrap(), WalletEvent::TxDetected(_)));
    let received = events.recv().await.unwrap();
    let WalletEvent::BalanceChanged(update) = received else {
        panic!("expected the coalesced balance, got {received:?}");
    };
    assert_eq!(update.balance, Amount::from_base_units(3));
    assert!(events.try_recv().is_err(), "the window should carry one balance");
}

#[tokio::test]
async fn every_detected_transaction_survives_its_window() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("lossless").await.unwrap();
    let added = harness.engine.add_address(CHAIN, None, None).await.unwrap();
    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);

    for n in 0..5 {
        sink.emit(WalletEvent::TxDetected(record(added.id, &format!("tx-{n}")))).await;
    }
    let mut seen = Vec::new();
    while seen.len() < 5 {
        if let WalletEvent::TxDetected(record) = events.recv().await.unwrap() {
            seen.push(record.txid.value().to_owned());
        }
    }
    assert_eq!(seen, vec!["tx-0", "tx-1", "tx-2", "tx-3", "tx-4"]);

    let page = harness.engine.list_txs(TxQuery { limit: 50, ..Default::default() }).await.unwrap();
    assert_eq!(page.rows.len(), 5);
}

async fn next_sightings(
    events: &mut broadcast::Receiver<WalletEvent>,
    count: usize,
) -> Fallible<Vec<(&'static str, String)>> {
    let mut sightings = Vec::new();
    while sightings.len() < count {
        match events.recv().await? {
            WalletEvent::TxDetected(record) => {
                sightings.push(("detected", record.txid.value().to_owned()));
            }
            WalletEvent::TxUpdated(record) => {
                sightings.push(("updated", record.txid.value().to_owned()));
            }
            WalletEvent::BalanceChanged(_)
            | WalletEvent::TxConfirmed(_)
            | WalletEvent::SyncProgress(_) => {}
        }
    }
    Ok(sightings)
}

/// A sync pass reads history it has already reported, and only the first sighting of
/// a transaction is news, whether the repeat shares its window or comes later.
#[tokio::test]
async fn only_the_first_sighting_of_a_transaction_is_announced_as_detected() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("sightings").await.unwrap();
    let added = harness.engine.add_address(CHAIN, None, None).await.unwrap();
    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);

    sink.emit(WalletEvent::TxDetected(record(added.id, "seen"))).await;
    sink.emit(WalletEvent::TxDetected(record(added.id, "seen"))).await;
    assert_eq!(
        next_sightings(&mut events, 2).await.unwrap(),
        vec![("detected", String::from("seen")), ("updated", String::from("seen"))]
    );

    sink.emit(WalletEvent::TxDetected(record(added.id, "seen"))).await;
    sink.emit(WalletEvent::TxDetected(record(added.id, "new"))).await;
    assert_eq!(
        next_sightings(&mut events, 2).await.unwrap(),
        vec![("updated", String::from("seen")), ("detected", String::from("new"))]
    );
}

#[tokio::test]
async fn a_rebroadcast_event_is_already_readable() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("ordered").await.unwrap();
    let added = harness.engine.add_address(CHAIN, None, None).await.unwrap();
    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);

    sink.emit(WalletEvent::TxDetected(record(added.id, "ordered-1"))).await;
    let txid = common::next_detected(&mut events).await.unwrap();
    assert_eq!(txid, "ordered-1");

    let query = TxQuery { txid: Some(txid), ..Default::default() };
    assert_eq!(harness.engine.list_txs(query).await.unwrap().rows.len(), 1);
}

#[tokio::test]
async fn a_confirmation_is_forwarded_without_a_row() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("confirming").await.unwrap();
    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);

    sink.emit(WalletEvent::TxConfirmed(TxConfirmation {
        txid: TxId::new(CHAIN, "somewhere"),
        confirmations: 3,
    }))
    .await;
    let received = events.recv().await.unwrap();
    let WalletEvent::TxConfirmed(confirmation) = received else {
        panic!("expected a confirmation, got {received:?}");
    };
    assert_eq!(confirmation.confirmations, 3);
    assert!(harness.engine.list_txs(TxQuery::default()).await.unwrap().rows.is_empty());
}

#[tokio::test]
async fn an_evm_reorg_confirmation_clears_the_mined_position_from_history() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("reorg").await.unwrap();
    for chain in [ChainId::Ethereum, ChainId::Bsc] {
        let added = harness.engine.add_address(chain, None, None).await.unwrap();
        let mut events = harness.engine.subscribe();
        let sink = test_util::event_sink(&harness.engine);
        let txid = format!("0x{}", "11".repeat(32));
        let mined = TxRecord::builder(
            AssetId::Native(chain),
            added.id,
            TxId::new(chain, &txid),
            TxDirection::Outgoing,
            Amount::from_base_units(1_000),
        )
        .confirmed_at(100, Some(1_700_000_000), 110)
        .build();
        sink.emit(WalletEvent::SyncProgress(
            ChainSyncProgress::new(chain).with_heights(Some(110), Some(110)),
        ))
        .await;
        let first_stored = harness.clock.now_secs().unwrap();
        sink.emit(WalletEvent::TxDetected(mined)).await;
        assert_eq!(common::next_detected(&mut events).await.unwrap(), txid);
        let query = TxQuery { chain: Some(chain), txid: Some(txid.clone()), ..TxQuery::default() };
        let page = harness.engine.list_txs(query.clone()).await.unwrap();
        let before = page.rows.first().unwrap();
        assert_eq!(
            (before.height, before.timestamp, before.confirmations),
            (Some(100), Some(1_700_000_000), 11)
        );

        harness.clock.advance(Duration::from_secs(60));
        sink.emit(WalletEvent::TxConfirmed(TxConfirmation {
            txid: TxId::new(chain, &txid),
            confirmations: 0,
        }))
        .await;
        loop {
            if let WalletEvent::TxConfirmed(confirmation) = events.recv().await.unwrap() {
                assert_eq!(confirmation.txid.chain(), chain);
                assert_eq!(confirmation.txid.value(), txid);
                assert_eq!(confirmation.confirmations, 0);
                break;
            }
        }

        let page = harness.engine.list_txs(query).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        let after = page.rows.first().unwrap();
        assert_eq!(
            (after.height, after.timestamp, after.confirmations),
            (None, Some(first_stored), 0),
            "a disappeared receipt must remove its obsolete inclusion from {chain} history and \
             leave its first sighting"
        );
    }
}

#[tokio::test]
async fn a_zcash_reorg_clears_the_orphaned_mined_position_from_history() {
    let harness = harness().await.unwrap();
    harness.seed_wallet("reorg").await.unwrap();
    let chain = ChainId::Zcash;
    let added = harness.engine.add_address(chain, None, None).await.unwrap();
    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);
    let txid = "22".repeat(32);
    let transfer = TxRecord::builder(
        AssetId::Native(chain),
        added.id,
        TxId::new(chain, &txid),
        TxDirection::Incoming,
        Amount::from_base_units(50_000),
    );
    let mined = transfer.clone().confirmed_at(100, Some(1_700_000_000), 110).build();
    sink.emit(WalletEvent::TxDetected(mined)).await;
    assert_eq!(common::next_detected(&mut events).await.unwrap(), txid);

    sink.emit(WalletEvent::TxConfirmed(TxConfirmation {
        txid: TxId::new(chain, &txid),
        confirmations: 0,
    }))
    .await;
    let orphaned = transfer.seen_at(1_700_000_000).outcome(TxOutcome::Unknown).build();
    sink.emit(WalletEvent::TxDetected(orphaned)).await;
    loop {
        if let WalletEvent::TxUpdated(record) = events.recv().await.unwrap()
            && record.txid.value() == txid
        {
            break;
        }
    }

    let query = TxQuery { chain: Some(chain), txid: Some(txid.clone()), ..TxQuery::default() };
    let page = harness.engine.list_txs(query).await.unwrap();
    let after = page.rows.first().unwrap();
    assert_eq!(
        (after.height, after.confirmations),
        (None, 0),
        "a zcash block that was reorganised away must stop counting as the transaction's \
         inclusion, but history still shows it mined at {:?} with {} confirmations",
        after.height,
        after.confirmations
    );
}

#[tokio::test]
async fn a_full_sink_drops_and_counts() {
    let tiny = EngineConfig {
        event_capacity: 1,
        event_flush: Duration::from_secs(30),
        ..test_util::config()
    };
    let harness = harness_with(tiny).await.unwrap();
    harness.seed_wallet("full").await.unwrap();
    let sink = test_util::event_sink(&harness.engine);

    assert_eq!(test_util::dropped_events(&harness.engine), 0);
    // The pump or the buffer takes the first one, and the rest cannot fit.
    for units in 0..64u128 {
        sink.try_emit(WalletEvent::BalanceChanged(BalanceUpdate {
            address_id: 1,
            asset: asset(),
            balance: Amount::from_base_units(units),
        }));
    }
    assert!(
        test_util::dropped_events(&harness.engine) > 0,
        "a bounded sink of one cannot hold sixty-four"
    );
}
