//! Sending from several addresses, and the history such transactions leave.

use jelly_core::{
    Amount, AssetId, ChainId, CoreError, FeeSelection, SendAmount, TxDirection, TxId, TxRecord,
    TxRequest, WalletEvent,
};
use jelly_engine::{TxEntry, TxQuery, test_util};

use crate::common::{Fallible, Harness, PASSWORD, harness, secret};

const CHAIN: ChainId = ChainId::Bitcoin;
const RECIPIENT: &str = "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4";

/// A wallet with `count` addresses on each of `chains`, every one handed to its
/// adapter, and their ids by chain in the order they were added.
async fn wallet(chains: &[ChainId], count: usize) -> Fallible<(Harness, Vec<Vec<i64>>)> {
    let harness = harness().await?;
    harness.builder.install(chains);
    harness.seed_wallet("pooled").await?;
    let mut ids = Vec::new();
    for &chain in chains {
        test_util::wait_for_pass(&harness.engine, chain, 0).await;
        let mut added = Vec::new();
        for _ in 0..count {
            added.push(harness.engine.add_address(chain, None, None).await?.id);
        }
        let passes = test_util::sync_passes(&harness.engine, chain);
        harness.engine.sync_now(Some(chain)).await?;
        test_util::wait_for_pass(&harness.engine, chain, passes).await;
        ids.push(added);
    }
    Ok((harness, ids))
}

fn send(chain: ChainId, sources: &[i64]) -> TxRequest {
    TxRequest {
        from_address_ids: sources.to_vec(),
        to: RECIPIENT.to_owned(),
        destination_tag: None,
        comment: None,
        should_delete_account: false,
        asset: AssetId::Native(chain),
        amount: SendAmount::Exact { value: Amount::from_base_units(10_000) },
        fee: FeeSelection::NORMAL,
        nonce: None,
        data: None,
    }
}

/// Both sources sign, and the send shows once in the wallet's history, recorded
/// under the first.
#[tokio::test]
async fn a_send_from_several_addresses_is_signed_by_each_and_recorded_once() {
    let (harness, ids) = wallet(&[CHAIN], 2).await.unwrap();
    let sources = ids[0].clone();
    let prepared = harness.engine.prepare_tx(send(CHAIN, &sources)).await.unwrap();
    assert_eq!(prepared.preview.from.len(), 2);
    let sent = harness
        .engine
        .submit_tx(prepared.id, secret(PASSWORD), prepared.preview_digest)
        .await
        .unwrap();
    assert_eq!(*harness.builder.state(CHAIN).signed_key_counts.lock(), [2]);

    let query = TxQuery { txid: Some(sent.value().to_owned()), ..TxQuery::default() };
    let rows = harness.engine.list_txs(query).await.unwrap().rows;
    let entry = only(&rows).unwrap();
    assert_eq!(entry.direction, TxDirection::Outgoing);
    assert_eq!(entry.sender_ids, [sources[0]]);
    assert_eq!(entry.amount, Amount::from_base_units(10_000));
    assert_eq!(entry.counterparty.as_deref(), Some(RECIPIENT));
}

#[tokio::test]
async fn a_send_names_its_sources_once_each_on_one_chain() {
    let (harness, ids) = wallet(&[CHAIN, ChainId::Litecoin], 2).await.unwrap();
    let (bitcoin, litecoin) = (&ids[0], &ids[1]);
    for (sources, wanted) in [
        (Vec::new(), "fromAddressIds"),
        (vec![bitcoin[0], bitcoin[0]], "fromAddressIds"),
        (vec![bitcoin[0], litecoin[0]], "asset"),
    ] {
        let refused = harness.engine.prepare_tx(send(CHAIN, &sources)).await;
        assert!(
            matches!(refused, Err(CoreError::InvalidInput { field, .. }) if field == wanted),
            "{sources:?}: {refused:?}"
        );
    }
}

/// Only a UTXO chain pools addresses. An account chain refuses a second source with
/// copy the interface shows.
#[tokio::test]
async fn an_account_chain_sends_from_exactly_one_address() {
    let (harness, ids) = wallet(&[ChainId::Ethereum], 2).await.unwrap();
    let refused = harness.engine.prepare_tx(send(ChainId::Ethereum, &ids[0])).await;
    let Err(CoreError::InvalidInput { field, detail }) = refused else {
        panic!("expected an invalid input refusal, got {refused:?}");
    };
    assert_eq!(field, "fromAddressIds");
    assert_eq!(detail, "Choose one address to send from.");
    let quote = harness.engine.quote_fees(send(ChainId::Ethereum, &ids[0])).await;
    assert!(matches!(quote, Err(CoreError::InvalidInput { .. })), "{quote:?}");
}

fn share(
    address_id: i64,
    txid: &str,
    direction: TxDirection,
    amount: u128,
    counterparty: &str,
) -> TxRecord {
    TxRecord::builder(
        AssetId::Native(CHAIN),
        address_id,
        TxId::new(CHAIN, txid),
        direction,
        Amount::from_base_units(amount),
    )
    .fee(Amount::from_base_units(1_000))
    .counterparty(counterparty)
    .confirmed_at(100, Some(1_700_000_000), 100)
    .build()
}

fn only(rows: &[TxEntry]) -> Fallible<&TxEntry> {
    match rows {
        [entry] => Ok(entry),
        _ => Err(format!("expected one entry, got {rows:?}").into()),
    }
}

/// A pooled send, a transfer between two of the wallet's addresses, and a payment to two of
/// them each list once in the wallet's history.
///
/// Each is combined from every address's share, and each address still lists its own share.
#[tokio::test]
async fn the_history_lists_a_shared_transaction_once_and_each_address_its_share() {
    let (harness, ids) = wallet(&[CHAIN], 2).await.unwrap();
    let (first, second) = (ids[0][0], ids[0][1]);
    let addresses = harness.engine.list_addresses(Some(CHAIN)).await.unwrap();
    let address_of =
        |id: i64| addresses.iter().find(|row| row.id == id).map(|row| row.address.clone()).unwrap();
    let mut events = harness.engine.subscribe();
    let sink = test_util::event_sink(&harness.engine);
    for record in [
        share(first, "pooled", TxDirection::Outgoing, 50_000, RECIPIENT),
        share(second, "pooled", TxDirection::Outgoing, 70_000, RECIPIENT),
        share(first, "moved", TxDirection::Outgoing, 5_000, &address_of(second)),
        share(second, "moved", TxDirection::Incoming, 5_000, &address_of(first)),
        share(first, "paid", TxDirection::Incoming, 3_000, RECIPIENT),
        share(second, "paid", TxDirection::Incoming, 4_000, RECIPIENT),
    ] {
        sink.emit(WalletEvent::TxDetected(record)).await;
    }
    let mut detected = Vec::new();
    let mut announced = 0;
    while announced < 6 {
        match events.recv().await.unwrap() {
            WalletEvent::TxDetected(record) => {
                detected.push(record.txid.value().to_owned());
                announced += 1;
            }
            WalletEvent::TxUpdated(_) => announced += 1,
            WalletEvent::BalanceChanged(_)
            | WalletEvent::TxConfirmed(_)
            | WalletEvent::SyncProgress(_) => {}
        }
    }
    detected.sort();
    assert_eq!(detected, ["moved", "paid", "pooled"], "another address's share is no news");

    let mut entries = Vec::new();
    for txid in ["pooled", "moved", "paid"] {
        let query = TxQuery { txid: Some(txid.to_owned()), ..TxQuery::default() };
        entries.push(only(&harness.engine.list_txs(query).await.unwrap().rows).unwrap().clone());
    }
    let [pooled, moved, paid] = entries.as_slice() else { panic!("three entries") };
    assert_eq!(
        (pooled.direction, pooled.amount, pooled.fee),
        (
            TxDirection::Outgoing,
            Amount::from_base_units(120_000),
            Some(Amount::from_base_units(1_000))
        )
    );
    assert_eq!(pooled.sender_ids, [first, second]);
    assert_eq!(pooled.counterparty.as_deref(), Some(RECIPIENT));
    assert_eq!(
        (moved.direction, moved.amount),
        (TxDirection::SelfTransfer, Amount::from_base_units(5_000))
    );
    assert_eq!((moved.sender_ids.clone(), moved.receiver_ids.clone()), (vec![first], vec![second]));
    assert_eq!(
        (paid.direction, paid.amount),
        (TxDirection::Incoming, Amount::from_base_units(7_000))
    );
    assert_eq!(paid.receiver_ids, [first, second]);

    let all = TxQuery { chain: Some(CHAIN), ..TxQuery::default() };
    assert_eq!(harness.engine.list_txs(all).await.unwrap().rows.len(), 3);
    let own = TxQuery { address_id: Some(second), ..TxQuery::default() };
    let mut shares: Vec<(String, TxDirection, Amount)> = harness
        .engine
        .list_txs(own)
        .await
        .unwrap()
        .rows
        .into_iter()
        .map(|row| (row.txid, row.direction, row.amount))
        .collect();
    shares.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(
        shares,
        [
            (String::from("moved"), TxDirection::Incoming, Amount::from_base_units(5_000)),
            (String::from("paid"), TxDirection::Incoming, Amount::from_base_units(4_000)),
            (String::from("pooled"), TxDirection::Outgoing, Amount::from_base_units(70_000)),
        ],
        "an address's own view keeps its own share"
    );
}
