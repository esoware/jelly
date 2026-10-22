use jelly_core::{Amount, AssetId, BalanceUpdate, ChainId, TokenId};
use jelly_store::{Store, StoreError, SyncBatch};

use crate::common;

async fn write(store: &Store, balances: Vec<BalanceUpdate>) -> Result<(), StoreError> {
    store.apply_sync_batch(SyncBatch { balances, ..SyncBatch::default() }).await.map(drop)
}

const fn update(address_id: i64, asset: AssetId, amount: u128) -> BalanceUpdate {
    BalanceUpdate { address_id, asset, balance: Amount::from_base_units(amount) }
}

#[tokio::test]
async fn a_balance_comes_back_with_its_address() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "balances").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qone").await.unwrap();

    write(&harness.store, vec![update(address.id, AssetId::Native(ChainId::Bitcoin), 123_456)])
        .await
        .unwrap();

    let rows = harness.store.list_balances(wallet.id).await.unwrap();
    let row = rows.first().expect("one row");
    assert_eq!(row.address_id, address.id);
    assert_eq!(row.asset, AssetId::Native(ChainId::Bitcoin));
    assert_eq!(row.amount, Amount::from_base_units(123_456));
}

#[tokio::test]
async fn writing_the_same_pair_twice_replaces_the_amount() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "replace").await.unwrap();
    let address =
        common::address(&harness.store, wallet.id, ChainId::Tron, "TAddress").await.unwrap();
    let asset = AssetId::Native(ChainId::Tron);

    write(&harness.store, vec![update(address.id, asset, 1)]).await.unwrap();
    write(&harness.store, vec![update(address.id, asset, 2)]).await.unwrap();

    let rows = harness.store.list_balances(wallet.id).await.unwrap();
    assert_eq!(rows.len(), 1, "the second write added a row instead of replacing one");
    assert_eq!(rows.first().map(|row| row.amount), Some(Amount::from_base_units(2)));
}

#[tokio::test]
async fn a_balance_for_an_address_that_is_not_there_is_refused() {
    let harness = common::harness().await.unwrap();
    let error = write(&harness.store, vec![update(404, AssetId::Native(ChainId::Bitcoin), 1)])
        .await
        .expect_err("no such address");
    assert!(matches!(error, StoreError::NotFound { entity: "address", .. }), "{error:?}");
}

#[tokio::test]
async fn totals_add_across_addresses_and_stay_per_asset() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "totals").await.unwrap();
    let first =
        common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xone").await.unwrap();
    let second =
        common::address(&harness.store, wallet.id, ChainId::Ethereum, "0xtwo").await.unwrap();
    let native = AssetId::Native(ChainId::Ethereum);
    let token = AssetId::Token(TokenId::UsdtErc20);

    write(
        &harness.store,
        vec![
            update(first.id, native, u128::from(u64::MAX)),
            update(second.id, native, 1),
            update(second.id, token, 9),
        ],
    )
    .await
    .unwrap();

    let totals = harness.store.balance_totals(wallet.id).await.unwrap();
    assert_eq!(totals.len(), 2);
    let native_total = totals.iter().find(|total| total.asset == native).expect("native");
    assert_eq!(
        native_total.amount,
        Amount::from_base_units(u128::from(u64::MAX).saturating_add(1))
    );
    let token_total = totals.iter().find(|total| total.asset == token).expect("token");
    assert_eq!(token_total.amount, Amount::from_base_units(9));
}

#[tokio::test]
async fn a_total_that_cannot_be_represented_is_refused() {
    let harness = common::harness().await.unwrap();
    let wallet = common::wallet(&harness.store, "overflow").await.unwrap();
    let first =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qmax").await.unwrap();
    let second =
        common::address(&harness.store, wallet.id, ChainId::Bitcoin, "bc1qmore").await.unwrap();
    let asset = AssetId::Native(ChainId::Bitcoin);

    write(&harness.store, vec![update(first.id, asset, u128::MAX), update(second.id, asset, 1)])
        .await
        .unwrap();

    let error = harness.store.balance_totals(wallet.id).await.expect_err("the total overflows");
    assert!(
        matches!(error, StoreError::Overflow { entity: "balance", column: "amount" }),
        "{error:?}"
    );
    assert_eq!(harness.store.list_balances(wallet.id).await.unwrap().len(), 2);
}
