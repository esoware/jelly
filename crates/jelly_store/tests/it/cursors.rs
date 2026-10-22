use jelly_core::{ChainId, CursorBytes, CursorScope, ReservationSlot, ScanAdvance, ScanStream};
use jelly_store::{Store, StoreError, SyncBatch};

use crate::common::{self, Fallible, scan};

async fn advance(
    store: &Store,
    address_id: i64,
    stream: ScanStream,
    value: &[u8],
) -> Result<(), StoreError> {
    store
        .apply_sync_batch(SyncBatch {
            scans: vec![ScanAdvance {
                scope: CursorScope::Address(address_id),
                stream,
                cursor: CursorBytes::copied(value),
            }],
            ..SyncBatch::default()
        })
        .await
        .map(drop)
}

async fn held(store: &Store, address_id: i64, slot: ReservationSlot) -> Fallible<Option<Vec<u8>>> {
    let rows = store.load_reservations(vec![CursorScope::Address(address_id)], vec![slot]).await?;
    Ok(rows.first().map(|row| row.payload.with_bytes(<[u8]>::to_vec)))
}

#[tokio::test]
async fn a_cursor_round_trips_and_is_replaced_in_place() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "cursors").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qcursor").await.unwrap();
    let stream = ScanStream::BitcoinElectrumStatus;

    advance(&harness.store, address.id, stream, b"first").await.unwrap();
    assert_eq!(scan(&harness.store, address.id, stream).await.unwrap(), Some(b"first".to_vec()));

    advance(&harness.store, address.id, stream, b"second").await.unwrap();
    assert_eq!(scan(&harness.store, address.id, stream).await.unwrap(), Some(b"second".to_vec()));
}

#[tokio::test]
async fn progress_and_a_reservation_do_not_share_a_slot() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "namespaces").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xcursor").await.unwrap();

    harness
        .store
        .reserve(
            CursorScope::Address(address.id),
            ReservationSlot::EvmNonces,
            CursorBytes::copied(b"nonces"),
        )
        .await
        .unwrap();
    advance(&harness.store, address.id, ScanStream::EvmLogScan, b"logs").await.unwrap();

    assert_eq!(
        scan(&harness.store, address.id, ScanStream::EvmLogScan).await.unwrap(),
        Some(b"logs".to_vec())
    );
    assert_eq!(
        held(&harness.store, address.id, ReservationSlot::EvmNonces).await.unwrap(),
        Some(b"nonces".to_vec())
    );
    assert_eq!(scan(&harness.store, address.id, ScanStream::MoneroScan).await.unwrap(), None);
    assert_eq!(held(&harness.store, address.id, ReservationSlot::TronSends).await.unwrap(), None);
}

#[tokio::test]
async fn a_load_reads_the_named_streams_of_many_addresses_at_once() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "load").await.unwrap();
    let first =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qfirst").await.unwrap();
    let second =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qsecond").await.unwrap();
    let third =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qthird").await.unwrap();
    let status = ScanStream::BitcoinElectrumStatus;

    advance(&harness.store, first.id, status, b"a").await.unwrap();
    advance(&harness.store, second.id, status, b"b").await.unwrap();
    advance(&harness.store, third.id, ScanStream::SolanaSignatures, b"c").await.unwrap();

    let scopes = [first.id, second.id, third.id].map(CursorScope::Address).to_vec();
    let rows = harness.store.load_scans(scopes, vec![status]).await.unwrap();
    assert_eq!(rows.len(), 2);
    let mut scopes: Vec<CursorScope> = rows.iter().map(|row| row.scope).collect();
    scopes.sort_unstable();
    assert_eq!(scopes, vec![CursorScope::Address(first.id), CursorScope::Address(second.id)]);
    for row in &rows {
        assert_eq!(row.stream, status);
    }

    let only_first = vec![CursorScope::Address(first.id)];
    assert!(harness.store.load_scans(Vec::new(), vec![status]).await.unwrap().is_empty());
    assert!(harness.store.load_scans(only_first.clone(), Vec::new()).await.unwrap().is_empty());
    assert!(harness.store.load_reservations(only_first, Vec::new()).await.unwrap().is_empty());
}

#[tokio::test]
async fn forgetting_reconstructible_scans_keeps_other_streams_addresses_and_reservations() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "forget").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Tron, "TCursor").await.unwrap();
    let other = common::address(&harness.store, wallet.id, ChainId::Tron, "TOther").await.unwrap();

    advance(&harness.store, address.id, ScanStream::TronTransfers, b"walked").await.unwrap();
    advance(&harness.store, other.id, ScanStream::TronTransfers, b"kept").await.unwrap();
    harness
        .store
        .reserve(
            CursorScope::Address(address.id),
            ReservationSlot::TronSends,
            CursorBytes::copied(b"sent"),
        )
        .await
        .unwrap();

    let protected = [
        ScanStream::BitcoinElectrumStatus,
        ScanStream::LitecoinElectrumStatus,
        ScanStream::DogecoinElectrumStatus,
        ScanStream::BitcoinCashElectrumStatus,
        ScanStream::EvmLogScan,
        ScanStream::ZcashTransactions,
        ScanStream::MoneroScan,
    ];
    for stream in protected {
        advance(&harness.store, address.id, stream, b"reconciliation").await.unwrap();
    }
    harness.store.forget_reconstructible_scans(vec![address.id]).await.unwrap();
    harness.store.forget_reconstructible_scans(vec![address.id]).await.unwrap();
    for stream in protected {
        assert_eq!(
            scan(&harness.store, address.id, stream).await.unwrap(),
            Some(b"reconciliation".to_vec())
        );
    }
    harness.store.forget_reconstructible_scans(Vec::new()).await.unwrap();

    assert_eq!(scan(&harness.store, address.id, ScanStream::TronTransfers).await.unwrap(), None);
    assert_eq!(
        scan(&harness.store, other.id, ScanStream::TronTransfers).await.unwrap(),
        Some(b"kept".to_vec())
    );
    assert_eq!(
        held(&harness.store, address.id, ReservationSlot::TronSends).await.unwrap(),
        Some(b"sent".to_vec())
    );
}

#[tokio::test]
async fn a_cursor_for_an_address_that_is_not_there_is_refused() {
    let harness = common::harness().await.unwrap();
    let error = advance(&harness.store, 404, ScanStream::MoneroScan, b"nope")
        .await
        .expect_err("no such address");
    assert!(matches!(error, StoreError::NotFound { entity: "address", .. }), "{error:?}");

    let error = harness
        .store
        .reserve(
            CursorScope::Address(404),
            ReservationSlot::MoneroSends,
            CursorBytes::copied(b"nope"),
        )
        .await
        .expect_err("no such address");
    assert!(matches!(error, StoreError::NotFound { entity: "address", .. }), "{error:?}");
}

#[tokio::test]
async fn a_wallet_scoped_cursor_has_no_address_row_to_live_under() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "wallet scope").await.unwrap();
    common::address(&harness.store, wallet.id, ChainId::Monero, "4Wallet").await.unwrap();

    let error = harness
        .store
        .apply_sync_batch(SyncBatch {
            scans: vec![ScanAdvance {
                scope: CursorScope::Wallet,
                stream: ScanStream::MoneroScan,
                cursor: CursorBytes::copied(b"scan"),
            }],
            ..SyncBatch::default()
        })
        .await
        .expect_err("nothing to file it under");
    assert!(matches!(error, StoreError::MissingReference { .. }), "{error:?}");
    let error = harness
        .store
        .reserve(CursorScope::Wallet, ReservationSlot::MoneroSends, CursorBytes::copied(b"sends"))
        .await
        .expect_err("nothing to file it under");
    assert!(matches!(error, StoreError::MissingReference { .. }), "{error:?}");

    let wallet_scope = vec![CursorScope::Wallet];
    assert!(
        harness
            .store
            .load_scans(wallet_scope.clone(), vec![ScanStream::MoneroScan])
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        harness
            .store
            .load_reservations(wallet_scope, vec![ReservationSlot::MoneroSends])
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn deleting_an_address_takes_its_cursors_with_it() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "cascade").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qcascade").await.unwrap();
    let stream = ScanStream::BitcoinElectrumStatus;
    advance(&harness.store, address.id, stream, b"gone").await.unwrap();

    harness.store.delete_address(address.id).await.unwrap();

    assert_eq!(scan(&harness.store, address.id, stream).await.unwrap(), None);
}
