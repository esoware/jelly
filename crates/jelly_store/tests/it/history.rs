use std::time::Duration;

use jelly_core::{
    Amount, AssetId, ChainId, Clock as _, TokenId, TxDetection, TxDirection, TxId, TxOutcome,
    TxRecord,
};
use jelly_store::{
    ChainStatePatch, Store, StoreError, SyncBatch, TxFilter, TxGroup, TxGroupFilter, TxRow, TxWrite,
};

use crate::common::{self, record};

async fn write(store: &Store, txs: Vec<TxRecord>) -> Result<Vec<TxWrite>, StoreError> {
    store.apply_sync_batch(SyncBatch { txs, ..SyncBatch::default() }).await
}

fn txids(rows: &[TxRow]) -> Vec<String> {
    rows.iter().map(|row| row.txid.clone()).collect()
}

#[tokio::test]
async fn a_record_round_trips_without_a_confirmation_count() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "records").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qrecords").await.unwrap();

    let stored = TxRecord::builder(
        AssetId::Native(ChainId::Bitcoin),
        address.id,
        TxId::new(ChainId::Bitcoin, "abc"),
        TxDirection::Outgoing,
        Amount::ZERO,
    )
    .fee(Amount::from_base_units(210))
    .rent(Amount::from_base_units(330))
    .counterparty("bc1qother")
    .outcome(TxOutcome::Failed)
    .detection(TxDetection::BalanceDelta)
    .confirmed_at(800_000, Some(1_700_000_000), 800_010)
    .build();
    write(&harness.store, vec![stored]).await.unwrap();

    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    let row = page.rows.first().expect("one row");
    assert_eq!(row.address_id, address.id);
    assert_eq!(row.txid, "abc");
    assert_eq!(row.chain, ChainId::Bitcoin);
    assert_eq!(row.asset, AssetId::Native(ChainId::Bitcoin));
    assert_eq!(row.direction, TxDirection::Outgoing);
    assert_eq!(row.outcome, TxOutcome::Failed);
    assert_eq!(row.detection, TxDetection::BalanceDelta);
    assert_eq!(row.amount, Amount::ZERO);
    assert_eq!(row.fee, Some(Amount::from_base_units(210)));
    assert_eq!(row.rent, Some(Amount::from_base_units(330)));
    assert_eq!(row.counterparty.as_deref(), Some("bc1qother"));
    assert_eq!(row.height, Some(800_000));
    assert_eq!(row.timestamp_unix_secs, Some(1_700_000_000));
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.next_cursor, None);
}

/// A send's own row carries the rent its review showed. A sighting that cannot tell
/// keeps it, and one that saw what the transaction really paid replaces it.
#[tokio::test]
async fn a_sighting_without_a_rent_keeps_the_one_stored() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "rent").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Solana, "owner").await.unwrap();
    let sighting = |rent: Option<u128>| {
        let mut builder = TxRecord::builder(
            AssetId::Token(TokenId::UsdtSpl),
            address.id,
            TxId::new(ChainId::Solana, "opening"),
            TxDirection::Outgoing,
            Amount::from_base_units(1_000_000),
        );
        if let Some(rent) = rent {
            builder = builder.rent(Amount::from_base_units(rent));
        }
        builder.build()
    };
    let stored_rent = async || {
        let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
        page.rows.first().expect("one row").rent
    };

    write(&harness.store, vec![sighting(Some(2_039_280))]).await.unwrap();
    write(&harness.store, vec![sighting(None)]).await.unwrap();
    assert_eq!(stored_rent().await, Some(Amount::from_base_units(2_039_280)));
    write(&harness.store, vec![sighting(Some(0))]).await.unwrap();
    assert_eq!(stored_rent().await, Some(Amount::ZERO));
}

#[tokio::test]
async fn a_speed_up_keeps_the_transaction_it_pays_for_through_later_sightings() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "speed").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Litecoin, "ltc1qspeed").await.unwrap();
    let child = |pays_for: Option<&str>, height: Option<u64>| {
        let mut builder = TxRecord::builder(
            AssetId::Native(ChainId::Litecoin),
            address.id,
            TxId::new(ChainId::Litecoin, "child"),
            TxDirection::SelfTransfer,
            Amount::ZERO,
        )
        .fee(Amount::from_base_units(9_000));
        if let Some(parent) = pays_for {
            builder = builder.pays_for(TxId::new(ChainId::Litecoin, parent));
        }
        if let Some(height) = height {
            builder = builder.confirmed_at(height, Some(1_700_000_000), height);
        }
        builder.build()
    };
    write(&harness.store, vec![child(Some("parent"), None)]).await.unwrap();
    write(&harness.store, vec![child(None, Some(2_800_000))]).await.unwrap();

    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    let row = page.rows.first().expect("one row");
    assert_eq!(row.pays_for.as_deref(), Some("parent"), "a later sighting cannot unsee it");
    assert_eq!(row.height, Some(2_800_000));
}

#[tokio::test]
async fn an_optimistic_send_does_not_erase_the_mined_position() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "confirm").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qconfirm").await.unwrap();

    write(&harness.store, vec![record(address.id, "pending", None)]).await.unwrap();
    write(&harness.store, vec![record(address.id, "pending", Some(1_700_000_500))]).await.unwrap();
    let mut optimistic = record(address.id, "pending", None);
    optimistic.outcome = TxOutcome::Unknown;
    write(&harness.store, vec![optimistic]).await.unwrap();

    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    assert_eq!(page.rows.len(), 1);
    let row = page.rows.first().expect("one row");
    assert_eq!(row.height, Some(800_000));
    assert_eq!(row.timestamp_unix_secs, Some(1_700_000_500));
}

#[tokio::test]
async fn a_utxo_returning_to_the_mempool_loses_its_mined_position() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "reorg").await.unwrap();
    for chain in [
        ChainId::Bitcoin,
        ChainId::Litecoin,
        ChainId::BitcoinCash,
        ChainId::Dogecoin,
        ChainId::Zcash,
    ] {
        let address = common::address(&harness.store, wallet.id, chain, "owned").await.unwrap();
        let sighting = TxRecord::builder(
            AssetId::Native(chain),
            address.id,
            TxId::new(chain, "reorganized"),
            TxDirection::Incoming,
            Amount::from_base_units(5_000),
        );
        let first_stored = harness.clock.now_secs().unwrap();
        write(
            &harness.store,
            vec![sighting.clone().confirmed_at(100, Some(1_700_000_000), 110).build()],
        )
        .await
        .unwrap();
        harness.clock.advance(Duration::from_secs(60));
        write(&harness.store, vec![sighting.clone().build()]).await.unwrap();
        let filter = TxFilter { address_id: Some(address.id), ..TxFilter::new(wallet.id) };
        let page = harness.store.list_txs(filter.clone()).await.unwrap();
        let row = page.rows.first().expect("the transaction returned to pending history");
        assert_eq!(row.height, None);
        assert_eq!(
            row.timestamp_unix_secs,
            Some(first_stored),
            "the block's time goes, and the first sighting shows"
        );
        write(&harness.store, vec![sighting.confirmed_at(111, Some(1_700_000_100), 111).build()])
            .await
            .unwrap();
        let page = harness.store.list_txs(filter).await.unwrap();
        assert_eq!(page.rows.len(), 1);
        let row = page.rows.first().unwrap();
        assert_eq!(row.height, Some(111));
        assert_eq!(row.timestamp_unix_secs, Some(1_700_000_100));
    }
}

#[tokio::test]
async fn a_mempool_sighting_keeps_the_time_a_send_was_broadcast() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "broadcast").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Litecoin, "ltc1qsend").await.unwrap();
    let send = TxRecord::builder(
        AssetId::Native(ChainId::Litecoin),
        address.id,
        TxId::new(ChainId::Litecoin, "sent"),
        TxDirection::Outgoing,
        Amount::from_base_units(5_000),
    );
    write(
        &harness.store,
        vec![send.clone().outcome(TxOutcome::Unknown).seen_at(1_700_000_000).build()],
    )
    .await
    .unwrap();
    write(&harness.store, vec![send.build()]).await.unwrap();

    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    let row = page.rows.first().expect("one row");
    assert_eq!(row.height, None);
    assert_eq!(row.timestamp_unix_secs, Some(1_700_000_000));
}

#[tokio::test]
async fn an_unmined_sighting_without_a_time_was_first_seen_when_stored() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "first-seen").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qseen").await.unwrap();
    let first_seen = harness.clock.now_secs().unwrap();
    write(&harness.store, vec![record(address.id, "incoming", None)]).await.unwrap();
    harness.clock.advance(Duration::from_secs(60));
    write(&harness.store, vec![record(address.id, "incoming", None)]).await.unwrap();

    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    let row = page.rows.first().expect("one row");
    assert_eq!(row.timestamp_unix_secs, Some(first_seen), "a later sighting does not move it");

    write(&harness.store, vec![record(address.id, "incoming", Some(1_700_000_500))]).await.unwrap();
    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    let row = page.rows.first().expect("one row");
    assert_eq!(row.timestamp_unix_secs, Some(1_700_000_500), "the block's time replaces it");
}

#[tokio::test]
async fn a_balance_delta_cannot_downgrade_a_decoded_self_transfer() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "decoding").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Solana, "owner").await.unwrap();
    let weak = TxRecord::builder(
        AssetId::Native(ChainId::Solana),
        address.id,
        TxId::new(ChainId::Solana, "inner-transfer"),
        TxDirection::Outgoing,
        Amount::ZERO,
    )
    .detection(TxDetection::BalanceDelta)
    .fee(Amount::from_base_units(5_000))
    .confirmed_at(102, None, 102)
    .build();
    let full = TxRecord::builder(
        AssetId::Native(ChainId::Solana),
        address.id,
        TxId::new(ChainId::Solana, "inner-transfer"),
        TxDirection::SelfTransfer,
        Amount::from_base_units(10_000),
    )
    .confirmed_at(101, Some(1_700_000_000), 101)
    .build();
    write(&harness.store, vec![weak.clone()]).await.unwrap();
    write(&harness.store, vec![full]).await.unwrap();
    write(&harness.store, vec![weak]).await.unwrap();
    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    let row = page.rows.first().unwrap();
    assert_eq!(row.direction, TxDirection::SelfTransfer);
    assert_eq!(row.detection, TxDetection::Full);
    assert_eq!(row.amount, Amount::from_base_units(10_000));
    assert_eq!(row.fee, Some(Amount::from_base_units(5_000)));
    assert_eq!(row.height, Some(102));
    assert_eq!(row.timestamp_unix_secs, Some(1_700_000_000));
}

#[tokio::test]
async fn a_write_says_whether_the_row_was_already_there() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "sightings").await.unwrap();
    let address = common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qsightings")
        .await
        .unwrap();
    let other = common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qelsewhere")
        .await
        .unwrap();

    let first = write(
        &harness.store,
        vec![record(address.id, "seen", None), record(address.id, "seen", Some(1_700_000_000))],
    )
    .await
    .unwrap();
    assert_eq!(first, vec![TxWrite::Inserted, TxWrite::Updated], "the repeat finds the row");

    let again = write(
        &harness.store,
        vec![
            record(address.id, "seen", Some(1_700_000_000)),
            record(other.id, "seen", None),
            record(other.id, "new", None),
        ],
    )
    .await
    .unwrap();
    assert_eq!(
        again,
        vec![TxWrite::Updated, TxWrite::Updated, TxWrite::Inserted],
        "another address's share of a known transaction is no news, and a new one is"
    );
}

/// An EVM token transfer is the owner's call to the token contract, which an indexer
/// also lists as a native transaction that moved nothing and paid the fee.
///
/// On the OP Stack an indexed row that got no fee from its receipt leaves out the L1
/// data fee.
#[tokio::test]
async fn a_call_to_a_token_contract_is_dropped_beside_the_token_row_that_paid_its_fee() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "calls").await.unwrap();
    let owner =
        common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xowner").await.unwrap();
    let elsewhere =
        common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xelsewhere").await.unwrap();
    let eth = AssetId::Native(ChainId::Ethereum);
    let usdt = AssetId::Token(TokenId::UsdtErc20);
    let row = |address_id: i64, asset: AssetId, txid: &str, amount: u128, fee: Option<u128>| {
        let counterparty = match asset {
            AssetId::Native(_) => TokenId::UsdtErc20.contract(),
            AssetId::Token(_) => "0xpeer",
        };
        let mut builder = TxRecord::builder(
            asset,
            address_id,
            TxId::new(ChainId::Ethereum, txid),
            TxDirection::Outgoing,
            Amount::from_base_units(amount),
        )
        .counterparty(counterparty);
        if let Some(fee) = fee {
            builder = builder.fee(Amount::from_base_units(fee));
        }
        builder.build()
    };

    write(&harness.store, vec![row(owner.id, eth, "call-first", 0, Some(9))]).await.unwrap();
    write(&harness.store, vec![row(owner.id, usdt, "call-first", 500, Some(9))]).await.unwrap();
    let token_first = write(
        &harness.store,
        vec![
            row(owner.id, usdt, "token-first", 500, Some(9)),
            row(owner.id, eth, "token-first", 0, Some(9)),
        ],
    )
    .await
    .unwrap();
    assert_eq!(token_first, vec![TxWrite::Inserted, TxWrite::Updated], "the call is no news");
    let mut reverted = vec![
        row(owner.id, eth, "reverted", 0, Some(9)),
        row(owner.id, usdt, "reverted", 0, Some(12)),
    ];
    for record in &mut reverted {
        record.outcome = TxOutcome::Failed;
    }
    write(&harness.store, reverted).await.unwrap();

    let mut router = row(owner.id, eth, "router", 0, Some(9));
    router.counterparty = Some(String::from("0xrouter"));
    write(
        &harness.store,
        vec![
            row(owner.id, usdt, "unpaid", 500, None),
            row(owner.id, eth, "unpaid", 0, Some(9)),
            row(owner.id, usdt, "moved", 500, Some(9)),
            row(owner.id, eth, "moved", 7, Some(9)),
            row(owner.id, usdt, "apart", 500, Some(9)),
            row(elsewhere.id, eth, "apart", 0, Some(9)),
            row(owner.id, usdt, "router", 500, Some(9)),
            router,
        ],
    )
    .await
    .unwrap();

    let filter = TxFilter { asset: Some(eth), ..TxFilter::new(wallet.id) };
    let mut kept = txids(&harness.store.list_txs(filter).await.unwrap().rows);
    kept.sort();
    assert_eq!(
        kept,
        ["apart", "moved", "router", "unpaid"],
        "a row that moved coin or called another contract, a token row with no fee, and \
         another address keep theirs"
    );
    let filter = TxFilter { asset: Some(usdt), ..TxFilter::new(wallet.id) };
    assert_eq!(harness.store.list_txs(filter).await.unwrap().rows.len(), 7);
}

#[tokio::test]
async fn an_unknown_outcome_does_not_erase_a_recorded_one() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "outcome").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qoutcome").await.unwrap();
    let sighting = |outcome: TxOutcome| {
        TxRecord::builder(
            AssetId::Native(ChainId::Bitcoin),
            address.id,
            TxId::new(ChainId::Bitcoin, "settled"),
            TxDirection::Outgoing,
            Amount::from_base_units(1_000),
        )
        .outcome(outcome)
        .build()
    };

    write(&harness.store, vec![sighting(TxOutcome::Failed)]).await.unwrap();
    write(&harness.store, vec![sighting(TxOutcome::Unknown)]).await.unwrap();

    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    let row = page.rows.first().expect("one row");
    assert_eq!(
        row.outcome,
        TxOutcome::Failed,
        "a sighting that does not know the outcome erased the one already recorded"
    );
}

#[tokio::test]
async fn a_record_against_an_address_that_is_not_there_is_refused() {
    let harness = common::harness().await.unwrap();
    let error =
        write(&harness.store, vec![record(404, "orphan", None)]).await.expect_err("no address");
    assert!(matches!(error, StoreError::NotFound { entity: "address", .. }), "{error:?}");
}

#[tokio::test]
async fn a_record_whose_asset_is_on_another_chain_is_refused() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "chains").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qchains").await.unwrap();
    let foreign = TxRecord::builder(
        AssetId::Token(TokenId::UsdtErc20),
        address.id,
        TxId::new(ChainId::Ethereum, "foreign"),
        TxDirection::Incoming,
        Amount::from_base_units(1),
    )
    .build();

    let error = write(&harness.store, vec![foreign]).await.expect_err("wrong chain");
    assert!(matches!(error, StoreError::Rejected { entity: "transaction", .. }), "{error:?}");
}

#[tokio::test]
async fn filters_narrow_by_chain_asset_address_txid_and_direction() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "filters").await.unwrap();
    let btc =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qfilters").await.unwrap();
    let eth =
        common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xfilters").await.unwrap();

    let token = TxRecord::builder(
        AssetId::Token(TokenId::UsdtErc20),
        eth.id,
        TxId::new(ChainId::Ethereum, "token"),
        TxDirection::Outgoing,
        Amount::from_base_units(5),
    )
    .confirmed_at(21_000_000, Some(1_700_000_100), 21_000_000)
    .build();
    let native = TxRecord::builder(
        AssetId::Native(ChainId::Ethereum),
        eth.id,
        TxId::new(ChainId::Ethereum, "native"),
        TxDirection::Incoming,
        Amount::from_base_units(7),
    )
    .confirmed_at(21_000_001, Some(1_700_000_200), 21_000_001)
    .build();
    write(
        &harness.store,
        vec![
            record(btc.id, "mined", Some(1_700_000_000)),
            record(btc.id, "unmined", None),
            token,
            native,
        ],
    )
    .await
    .unwrap();

    let base = TxFilter::new(wallet.id);
    assert_eq!(harness.store.list_txs(base.clone()).await.unwrap().rows.len(), 4);

    let by_chain = TxFilter { chain: Some(ChainId::Bitcoin), ..base.clone() };
    assert_eq!(harness.store.list_txs(by_chain).await.unwrap().rows.len(), 2);

    let by_asset = TxFilter { asset: Some(AssetId::Token(TokenId::UsdtErc20)), ..base.clone() };
    assert_eq!(txids(&harness.store.list_txs(by_asset).await.unwrap().rows), vec!["token"]);

    let by_address = TxFilter { address_id: Some(eth.id), ..base.clone() };
    assert_eq!(harness.store.list_txs(by_address).await.unwrap().rows.len(), 2);

    let by_txid = TxFilter { txid: Some(String::from("mined")), ..base.clone() };
    assert_eq!(txids(&harness.store.list_txs(by_txid).await.unwrap().rows), vec!["mined"]);

    let by_direction = TxFilter { direction: Some(TxDirection::Outgoing), ..base };
    assert_eq!(txids(&harness.store.list_txs(by_direction).await.unwrap().rows), vec!["token"]);
}

#[tokio::test]
async fn a_record_with_no_time_of_its_own_is_ordered_by_when_it_was_seen() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "ordering").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qorder").await.unwrap();

    // A transaction detected now with no timestamp belongs above old mined history.
    write(&harness.store, vec![record(address.id, "old", Some(1_500_000_000))]).await.unwrap();
    write(&harness.store, vec![record(address.id, "just-seen", None)]).await.unwrap();

    let page = harness.store.list_txs(TxFilter::new(wallet.id)).await.unwrap();
    assert_eq!(txids(&page.rows), vec!["just-seen", "old"]);
}

#[tokio::test]
async fn pages_walk_newest_first() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "pages").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qpages").await.unwrap();

    let mut rows = Vec::new();
    for n in 0..6i64 {
        rows.push(record(address.id, &format!("mined-{n}"), Some(1_700_000_000 + n)));
    }
    write(&harness.store, rows).await.unwrap();

    let page =
        harness.store.list_txs(TxFilter { limit: 2, ..TxFilter::new(wallet.id) }).await.unwrap();
    assert_eq!(txids(&page.rows), vec!["mined-5", "mined-4"]);

    let mut seen = txids(&page.rows);
    let mut next = page.next_cursor;
    while let Some(position) = next {
        let page = harness
            .store
            .list_txs(TxFilter { limit: 2, cursor: Some(position), ..TxFilter::new(wallet.id) })
            .await
            .unwrap();
        seen.extend(txids(&page.rows));
        next = page.next_cursor;
    }
    assert_eq!(seen, vec!["mined-5", "mined-4", "mined-3", "mined-2", "mined-1", "mined-0"]);
}

#[tokio::test]
async fn the_last_page_reports_no_next_position() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "exact").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qexact").await.unwrap();
    write(
        &harness.store,
        vec![
            record(address.id, "one", Some(1_700_000_000)),
            record(address.id, "two", Some(1_700_000_001)),
        ],
    )
    .await
    .unwrap();

    let page =
        harness.store.list_txs(TxFilter { limit: 2, ..TxFilter::new(wallet.id) }).await.unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.next_cursor, None, "a full page that ends the set has no next position");
}

#[tokio::test]
async fn a_row_inserted_between_pages_is_neither_skipped_nor_repeated() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "stability").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qstable").await.unwrap();

    let mut rows = Vec::new();
    for n in 0..4i64 {
        rows.push(record(address.id, &format!("old-{n}"), Some(1_700_000_000 + n)));
    }
    write(&harness.store, rows).await.unwrap();

    let first =
        harness.store.list_txs(TxFilter { limit: 2, ..TxFilter::new(wallet.id) }).await.unwrap();
    assert_eq!(txids(&first.rows), vec!["old-3", "old-2"]);
    let cursor = first.next_cursor.expect("there is more");

    write(&harness.store, vec![record(address.id, "fresh", Some(1_700_000_900))]).await.unwrap();

    let second = harness
        .store
        .list_txs(TxFilter { limit: 10, cursor: Some(cursor), ..TxFilter::new(wallet.id) })
        .await
        .unwrap();
    assert_eq!(txids(&second.rows), vec!["old-1", "old-0"]);
    assert_eq!(second.next_cursor, None);
}

#[tokio::test]
async fn rows_that_share_a_timestamp_still_page_apart() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "ties").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qties").await.unwrap();

    let mut rows = Vec::new();
    for n in 0..4i64 {
        rows.push(record(address.id, &format!("tied-{n}"), Some(1_700_000_000)));
    }
    write(&harness.store, rows).await.unwrap();

    let mut seen = Vec::new();
    let mut next = None;
    loop {
        let page = harness
            .store
            .list_txs(TxFilter { limit: 1, cursor: next, ..TxFilter::new(wallet.id) })
            .await
            .unwrap();
        seen.extend(txids(&page.rows));
        next = page.next_cursor;
        if next.is_none() {
            break;
        }
    }
    seen.sort_unstable();
    assert_eq!(seen, vec!["tied-0", "tied-1", "tied-2", "tied-3"]);
}

#[tokio::test]
async fn a_limit_outside_the_allowed_range_is_clamped() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "limits").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qlimits").await.unwrap();
    let mut rows = Vec::new();
    for n in 0..=i64::from(TxFilter::MAX_LIMIT) {
        rows.push(record(address.id, &format!("row-{n}"), Some(1_700_000_000 + n)));
    }
    write(&harness.store, rows).await.unwrap();

    let zero_limit =
        harness.store.list_txs(TxFilter { limit: 0, ..TxFilter::new(wallet.id) }).await.unwrap();
    assert_eq!(zero_limit.rows.len(), 1, "a zero limit still returns a page");

    let huge = harness
        .store
        .list_txs(TxFilter { limit: u32::MAX, ..TxFilter::new(wallet.id) })
        .await
        .unwrap();
    assert_eq!(huge.rows.len(), usize::try_from(TxFilter::MAX_LIMIT).unwrap());
    assert!(huge.next_cursor.is_some(), "the row past the cap is left for the next page");
}

/// One of the wallet's Bitcoin rows of `txid` for `address_id`, mined when `timestamp`
/// is given.
fn share(
    address_id: i64,
    txid: &str,
    direction: TxDirection,
    amount: u128,
    counterparty: Option<&str>,
    timestamp: Option<i64>,
) -> TxRecord {
    let mut builder = TxRecord::builder(
        AssetId::Native(ChainId::Bitcoin),
        address_id,
        TxId::new(ChainId::Bitcoin, txid),
        direction,
        Amount::from_base_units(amount),
    )
    .fee(Amount::from_base_units(1_000));
    if let Some(counterparty) = counterparty {
        builder = builder.counterparty(counterparty);
    }
    match timestamp {
        Some(stamp) => builder.confirmed_at(800_000, Some(stamp), 800_000).build(),
        None => builder.build(),
    }
}

/// Every transaction of the wallet, a page of the default size from the newest.
const fn group_filter(wallet_id: i64) -> TxGroupFilter {
    TxGroupFilter {
        wallet_id,
        chain: None,
        asset: None,
        txid: None,
        direction: None,
        cursor: None,
        limit: TxFilter::DEFAULT_LIMIT,
    }
}

fn summary(group: &TxGroup) -> (String, TxDirection, Vec<i64>) {
    let txid = group.members.first().map(|member| member.row.txid.clone()).unwrap_or_default();
    (txid, group.direction, group.members.iter().map(|member| member.row.address_id).collect())
}

/// A transaction several of the wallet's addresses share lists once.
///
/// A send pooled from two addresses, a transfer between two of the wallet's addresses,
/// and a payment to two of them each take the direction the wallet's rows give the whole
/// transaction, while an address's own rows stay apart.
#[tokio::test]
async fn a_transaction_several_addresses_share_lists_once_with_the_wallets_direction() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "grouped").await.unwrap();
    let first =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qfirst").await.unwrap();
    let second =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qsecond").await.unwrap();
    let stranger = "bc1qstranger";
    write(
        &harness.store,
        vec![
            share(first.id, "pooled", TxDirection::Outgoing, 50_000, Some(stranger), Some(10)),
            share(second.id, "pooled", TxDirection::Outgoing, 70_000, Some(stranger), Some(10)),
            share(first.id, "moved", TxDirection::Outgoing, 5_000, Some("bc1qsecond"), Some(20)),
            share(second.id, "moved", TxDirection::Incoming, 5_000, Some("bc1qfirst"), Some(20)),
            share(first.id, "paid", TxDirection::Incoming, 3_000, Some(stranger), Some(30)),
            share(second.id, "paid", TxDirection::Incoming, 4_000, Some(stranger), Some(30)),
        ],
    )
    .await
    .unwrap();

    let page = harness.store.list_tx_groups(group_filter(wallet.id)).await.unwrap();
    assert_eq!(page.groups.len(), 3);
    let both = vec![first.id, second.id];
    assert_eq!(
        page.groups.iter().map(summary).collect::<Vec<_>>(),
        [
            (String::from("paid"), TxDirection::Incoming, both.clone()),
            (String::from("moved"), TxDirection::SelfTransfer, both.clone()),
            (String::from("pooled"), TxDirection::Outgoing, both),
        ]
    );
    assert!(
        page.groups.iter().flat_map(|group| &group.members).all(|member| {
            member.is_counterparty_own
                == matches!(member.row.counterparty.as_deref(), Some("bc1qfirst" | "bc1qsecond"))
        }),
        "a member knows whether it names another address of the wallet's"
    );

    let rows = harness
        .store
        .list_txs(TxFilter { address_id: Some(second.id), ..TxFilter::new(wallet.id) })
        .await
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 3, "an address's view keeps its own rows");

    for (direction, txid) in [
        (TxDirection::Outgoing, "pooled"),
        (TxDirection::SelfTransfer, "moved"),
        (TxDirection::Incoming, "paid"),
    ] {
        let filtered = harness
            .store
            .list_tx_groups(TxGroupFilter { direction: Some(direction), ..group_filter(wallet.id) })
            .await
            .unwrap();
        assert_eq!(filtered.groups.len(), 1, "{direction:?}");
        assert_eq!(filtered.groups.first().map(summary).map(|(id, ..)| id).as_deref(), Some(txid));
    }
}

#[tokio::test]
async fn a_group_pages_by_its_first_row_and_keeps_its_mined_position() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "paging").await.unwrap();
    let first =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qone").await.unwrap();
    let second =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qtwo").await.unwrap();
    write(
        &harness.store,
        vec![share(first.id, "early", TxDirection::Outgoing, 1, Some("x"), None)],
    )
    .await
    .unwrap();
    harness.clock.advance(Duration::from_secs(60));
    write(&harness.store, vec![share(first.id, "late", TxDirection::Incoming, 1, Some("x"), None)])
        .await
        .unwrap();
    harness.clock.advance(Duration::from_secs(60));
    write(
        &harness.store,
        vec![share(second.id, "early", TxDirection::Incoming, 1, Some("x"), None)],
    )
    .await
    .unwrap();

    let filter = group_filter(wallet.id);
    let page = harness.store.list_tx_groups(filter.clone()).await.unwrap();
    let order: Vec<String> = page.groups.iter().map(|group| summary(group).0).collect();
    assert_eq!(order, ["late", "early"], "the early transaction keeps its place");

    let one = TxGroupFilter { limit: 1, ..filter.clone() };
    let first_page = harness.store.list_tx_groups(one.clone()).await.unwrap();
    let rest = harness
        .store
        .list_tx_groups(TxGroupFilter { cursor: first_page.next_cursor, ..one })
        .await
        .unwrap();
    assert_eq!(rest.groups.iter().map(|group| summary(group).0).collect::<Vec<_>>(), ["early"]);
    assert_eq!(rest.next_cursor, None);

    write(
        &harness.store,
        vec![share(first.id, "early", TxDirection::Outgoing, 1, Some("x"), Some(5))],
    )
    .await
    .unwrap();
    let page = harness.store.list_tx_groups(filter).await.unwrap();
    assert_eq!(page.groups.len(), 2);
    let early = page.groups.iter().find(|group| summary(group).0 == "early").unwrap();
    assert!(early.members.iter().any(|member| member.row.height == Some(800_000)));
}

/// The switched-off chain holds the newest transactions, more than a page of them.
#[tokio::test]
async fn a_list_naming_no_chain_pages_past_a_chain_the_wallet_switched_off() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "switched").await.unwrap();
    let kept =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qkept").await.unwrap();
    let off =
        common::address(&harness.store, wallet.id, ChainId::Litecoin, "ltc1qoff").await.unwrap();
    let mut txs = vec![record(kept.id, "kept", Some(1_700_000_000))];
    for n in 1..=3 {
        let txid = format!("off-{n}");
        txs.push(
            TxRecord::builder(
                AssetId::Native(ChainId::Litecoin),
                off.id,
                TxId::new(ChainId::Litecoin, &txid),
                TxDirection::Incoming,
                Amount::from_base_units(1_000),
            )
            .confirmed_at(3_000_000, Some(1_700_000_000 + n), 3_000_000)
            .build(),
        );
    }
    write(&harness.store, txs).await.unwrap();
    let switched_off = ChainStatePatch { enabled: Some(false), ..ChainStatePatch::default() };
    harness.store.set_chain_state(wallet.id, ChainId::Litecoin, switched_off).await.unwrap();

    let limit = 2;
    let groups = harness
        .store
        .list_tx_groups(TxGroupFilter { limit, ..group_filter(wallet.id) })
        .await
        .unwrap();
    assert_eq!(groups.groups.iter().map(|group| summary(group).0).collect::<Vec<_>>(), ["kept"]);
    assert_eq!(groups.next_cursor, None);
    let rows =
        harness.store.list_txs(TxFilter { limit, ..TxFilter::new(wallet.id) }).await.unwrap();
    assert_eq!(txids(&rows.rows), ["kept"]);
    assert_eq!(rows.next_cursor, None);

    let named = TxGroupFilter { chain: Some(ChainId::Litecoin), ..group_filter(wallet.id) };
    let named = harness.store.list_tx_groups(named).await.unwrap();
    assert_eq!(named.groups.len(), 3, "a list that names the chain still reads it");
}
