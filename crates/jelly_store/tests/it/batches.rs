use jelly_core::{
    Amount, AssetId, BalanceUpdate, ChainId, CursorBytes, CursorScope, ScanAdvance, ScanStream,
    TxDirection, TxId, TxRecord,
};
use jelly_store::{StoreError, SyncBatch, TxFilter};

use crate::common::{self, record};

#[tokio::test]
async fn one_batch_commits_records_balances_and_progress_together() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "batch").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qbatch").await.unwrap();
    let stream = ScanStream::BitcoinElectrumStatus;

    let batch = SyncBatch {
        txs: vec![record(address.id, "batched", Some(1_700_000_000))],
        balances: vec![BalanceUpdate {
            address_id: address.id,
            asset: AssetId::Native(ChainId::Bitcoin),
            balance: Amount::from_base_units(7),
        }],
        scans: vec![ScanAdvance {
            scope: CursorScope::Address(address.id),
            stream,
            cursor: CursorBytes::copied(b"height:800000"),
        }],
    };
    assert!(!batch.is_empty());
    harness.store.apply_sync_batch(batch).await.unwrap();

    assert_eq!(harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap().rows.len(), 1);
    assert_eq!(harness.store.list_balances(wallet.id).await.unwrap().len(), 1);
    assert_eq!(
        common::scan(&harness.store, address.id, stream).await.unwrap(),
        Some(b"height:800000".to_vec())
    );
}

#[tokio::test]
async fn an_empty_batch_is_empty_and_applies() {
    let harness = common::harness().await.unwrap();
    assert!(SyncBatch::default().is_empty());
    let writes = harness.store.apply_sync_batch(SyncBatch::default()).await.unwrap();
    assert!(writes.is_empty());
}

#[tokio::test]
async fn a_batch_that_names_a_missing_address_writes_nothing_at_all() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "atomic").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qatomic").await.unwrap();
    let stream = ScanStream::BitcoinElectrumStatus;

    let batch = SyncBatch {
        txs: vec![record(address.id, "good", Some(1_700_000_000)), record(404, "orphan", None)],
        scans: vec![ScanAdvance {
            scope: CursorScope::Address(address.id),
            stream,
            cursor: CursorBytes::copied(b"height:900000"),
        }],
        ..SyncBatch::default()
    };
    let error = harness.store.apply_sync_batch(batch).await.expect_err("the batch is refused");
    assert!(matches!(error, StoreError::NotFound { entity: "address", .. }), "{error:?}");

    assert!(harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap().rows.is_empty());
    assert_eq!(common::scan(&harness.store, address.id, stream).await.unwrap(), None);
}

#[tokio::test]
async fn a_late_batch_for_a_deleted_address_never_lands_on_a_later_address() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "reuse").await.unwrap();
    common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xkept").await.unwrap();
    let deleted =
        common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xdeleted").await.unwrap();
    harness.store.delete_address(deleted.id).await.unwrap();
    let successor =
        common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xsuccessor").await.unwrap();

    // A sync pass that started before the delete reports this at its end.
    let stream = ScanStream::EvmLogScan;
    let late = SyncBatch {
        txs: vec![
            TxRecord::builder(
                AssetId::Native(ChainId::Ethereum),
                deleted.id,
                TxId::new(ChainId::Ethereum, "0xreceivedbythedeletedaddress"),
                TxDirection::Incoming,
                Amount::from_base_units(1_000_000_000_000_000_000),
            )
            .confirmed_at(21_000_000, Some(1_700_000_000), 21_000_000)
            .build(),
        ],
        balances: vec![BalanceUpdate {
            address_id: deleted.id,
            asset: AssetId::Native(ChainId::Ethereum),
            balance: Amount::from_base_units(1_000_000_000_000_000_000),
        }],
        scans: vec![ScanAdvance {
            scope: CursorScope::Address(deleted.id),
            stream,
            cursor: CursorBytes::copied(b"scanned to 21000000"),
        }],
    };
    let _outcome = harness.store.apply_sync_batch(late).await;

    let history = harness
        .store
        .list_txs(TxFilter { address_id: Some(successor.id), ..TxFilter::new(wallet.id) })
        .await
        .unwrap();
    let txids: Vec<String> = history.rows.into_iter().map(|row| row.txid).collect();
    assert!(
        txids.is_empty(),
        "expected the new address {successor_address} to have no history, but the deleted \
         address's late sync rows were filed under it because it was given the deleted row's \
         id {id}: {txids:?}",
        successor_address = successor.address,
        id = deleted.id,
    );
    assert_eq!(
        common::scan(&harness.store, successor.id, stream).await.unwrap(),
        None,
        "expected the new address to start its scan from the beginning, but it inherited the \
         deleted address's progress"
    );
}

#[tokio::test]
async fn a_failed_scope_rolls_back_a_batch_applied_inside_it() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "crash").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qcrash").await.unwrap();
    let stream = ScanStream::BitcoinElectrumStatus;
    let address_id = address.id;

    let error = harness
        .store
        .transact(move |writer| {
            writer
                .apply_sync_batch(&SyncBatch {
                    txs: vec![record(address_id, "half-written", Some(1_700_000_000))],
                    scans: vec![ScanAdvance {
                        scope: CursorScope::Address(address_id),
                        stream,
                        cursor: CursorBytes::copied(b"height:900001"),
                    }],
                    ..SyncBatch::default()
                })
                .unwrap();
            Err::<(), StoreError>(StoreError::Internal { context: "the process died here" })
        })
        .await
        .expect_err("the transaction never commits");
    assert!(matches!(error, StoreError::Internal { .. }), "{error:?}");

    assert!(harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap().rows.is_empty());
    assert_eq!(common::scan(&harness.store, address.id, stream).await.unwrap(), None);
}
