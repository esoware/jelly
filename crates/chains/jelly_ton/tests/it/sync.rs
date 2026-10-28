use std::time::Duration;

use jelly_core::{
    AssetId, ChainAdapter as _, ChainError, ChainId, Clock as _, CursorBytes, CursorScope,
    CursorStore as _, ScanStream, ScriptKind, TokenId, TxDirection, TxRecord, WalletEvent,
    test_util::FakeEventSink,
};
use jelly_ton::Method;
use serde_json::{Value, json};
use tonlib_core::types::TonAddress;

use super::common::{
    Node, SECRET, USDT_OWNER, USDT_SECRET, USDT_WALLET, ZERO_HASH, assert_refused, history_watched,
    watched, watched_as,
};

/// The USDT balances `events` reported since it was last drained, in order.
fn usdt_balances(events: &FakeEventSink) -> Vec<u128> {
    events
        .drain()
        .into_iter()
        .filter_map(|event| {
            if let WalletEvent::BalanceChanged(balance) = event
                && balance.asset == AssetId::Token(TokenId::UsdtJetton)
            {
                Some(balance.balance.base_units())
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test]
async fn an_account_without_history_syncs_its_balances_and_reads_no_history() {
    let node = Node::new();
    node.state.lock().account_status = String::from("uninitialized");
    let adapter = node.adapter().await.unwrap();
    let events = FakeEventSink::new();
    let progress =
        adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert!(progress.is_complete());
    assert_eq!(progress.tip_height(), Some(100));
    let drained = events.drain();
    assert!(drained.iter().any(|event| matches!(event, WalletEvent::BalanceChanged(balance) if balance.balance.base_units() == 5_000_000_000)));
    for token in [TokenId::EnaJetton, TokenId::UsdtJetton, TokenId::XautJetton] {
        assert!(
            drained.iter().any(|event| matches!(event, WalletEvent::BalanceChanged(balance)
                if balance.asset == AssetId::Token(token) && balance.balance.is_zero())),
            "an undeployed jetton wallet reads as zero"
        );
    }
    assert_eq!(node.count(Method::GetTransactions), 0);
}

#[tokio::test]
async fn jetton_wallets_are_resolved_once_and_read_only_when_they_change() {
    let node = Node::new();
    node.state.lock().jetton(TokenId::UsdtJetton).unwrap().wallet =
        TonAddress::from_base64_url(USDT_WALLET).unwrap();
    let adapter = node.adapter().await.unwrap();
    let events = FakeEventSink::new();
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(node.count(Method::RunGet), 3, "one get_wallet_address per registry master");
    let rows: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/usdt_wallet.json")).unwrap();
    {
        let mut state = node.state.lock();
        let jetton = state.jetton(TokenId::UsdtJetton).unwrap();
        jetton.status = String::from("active");
        jetton.balance = 42;
        jetton.position = rows[0]["transaction_id"].clone();
        jetton.pages.push_back(json!(rows));
    }
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(node.count(Method::RunGet), 4, "a changed jetton wallet is read once");
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(node.count(Method::RunGet), 4, "an unchanged one is not read again");
    assert_eq!(usdt_balances(&events), [0, 42, 42]);
}

#[tokio::test]
async fn a_frozen_jetton_wallet_keeps_its_verified_balance() {
    let node = Node::new();
    {
        let mut state = node.state.lock();
        let jetton = state.jetton(TokenId::UsdtJetton).unwrap();
        jetton.status = String::from("active");
        jetton.balance = 42;
    }
    let adapter = node.adapter().await.unwrap();
    let events = FakeEventSink::new();
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    events.drain();
    node.state.lock().jetton(TokenId::UsdtJetton).unwrap().status = String::from("frozen");
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert_eq!(usdt_balances(&events), [42]);
}

#[tokio::test]
async fn paginated_history_resumes_from_linked_cells_without_skipping_transactions() {
    let node = Node::new();
    let adapter = node.adapter_watching(history_watched()).await.unwrap();
    let rows: Vec<Value> = serde_json::from_str(include_str!("../fixtures/history.json")).unwrap();
    {
        let mut state = node.state.lock();
        state.position = rows.first().unwrap().get("transaction_id").unwrap().clone();
        state.pages.push_back(json!([rows.first().unwrap()]));
        state.pages.push_back(json!([&rows[1]]));
    }
    let cursor = json!({
        "version": 2,
        "wallet": {"latest_lt": 50_377_299_000_002u64, "head_lt": 0, "stop_lt": 0, "next": null},
        "jettons": [],
        "returns": {"sends": [], "orphans": []},
    });
    node.cursors
        .advance(
            CursorScope::Address(1),
            ScanStream::TonTransactions,
            CursorBytes::new(serde_json::to_vec(&cursor).unwrap()),
        )
        .await
        .unwrap();
    let events = FakeEventSink::new();
    let progress =
        adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert!(progress.is_complete());
    assert_eq!(events.records().len(), 2);
    adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap();
    assert!(events.records().is_empty());
}

/// The wallet-core test key's V5R1 wallet and its USDT jetton wallet, read from their
/// real mainnet histories through the adapter.
#[tokio::test]
async fn a_wallets_jetton_activity_is_read_from_both_histories() {
    let watched = watched_as(USDT_SECRET, ScriptKind::TonV5r1).unwrap();
    assert_eq!(watched.address.value(), USDT_OWNER);
    let node = Node::new();
    let wallet_rows: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/usdt_owner.json")).unwrap();
    let jetton_rows: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/usdt_wallet.json")).unwrap();
    {
        let mut state = node.state.lock();
        state.position = wallet_rows[0]["transaction_id"].clone();
        state.pages.push_back(json!(wallet_rows));
        let jetton = state.jetton(TokenId::UsdtJetton).unwrap();
        jetton.wallet = TonAddress::from_base64_url(USDT_WALLET).unwrap();
        jetton.status = String::from("active");
        jetton.position = jetton_rows[0]["transaction_id"].clone();
        jetton.pages.push_back(json!(jetton_rows));
    }
    let adapter = node.adapter_watching(watched).await.unwrap();
    let events = FakeEventSink::new();
    let progress =
        adapter.sync(node.clock.deadline_in(Duration::from_secs(60)), &events).await.unwrap();
    assert!(progress.is_complete());
    let read = events.records();
    let usdt = AssetId::Token(TokenId::UsdtJetton);
    let gram: Vec<&TxRecord> =
        read.iter().filter(|record| record.asset == AssetId::Native(ChainId::Ton)).collect();
    assert_eq!(gram.len(), 1, "only the funding is a Gram record: {gram:?}");
    assert_eq!(gram[0].amount.base_units(), 220_000_000);
    let sent: Vec<(u128, u128)> = read
        .iter()
        .filter(|record| record.asset == usdt && record.direction == TxDirection::Outgoing)
        .map(|record| (record.amount.base_units(), record.fee.unwrap().base_units()))
        .collect();
    assert_eq!(sent, [(110_000, 7_458_233), (120_000, 16_191_619)]);
    let arrived: Vec<u128> = read
        .iter()
        .filter(|record| record.asset == usdt && record.direction == TxDirection::Incoming)
        .map(|record| record.amount.base_units())
        .collect();
    assert_eq!(arrived, [230_000]);
    adapter.sync(node.clock.deadline_in(Duration::from_secs(60)), &events).await.unwrap();
    assert!(events.records().is_empty(), "a second pass reads nothing new");
}

#[tokio::test]
async fn missing_history_pages_are_errors_not_completed_scans() {
    let node = Node::new();
    node.state.lock().position = json!({"lt":"1000","hash":ZERO_HASH});
    let adapter = node.adapter().await.unwrap();
    let events = FakeEventSink::new();
    let error =
        adapter.sync(node.clock.deadline_in(Duration::from_secs(30)), &events).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
}

#[tokio::test]
async fn watched_keys_must_derive_the_source_address() {
    let node = Node::new();
    let adapter = node.adapter().await.unwrap();
    let mut address = watched().unwrap();
    address.pubkey = Some([9; 32]);
    assert_refused(
        &adapter.set_watch_set(&[address]).await.unwrap_err(),
        "The TON source address does not match its wallet key.",
    );
    for script in [ScriptKind::TonV4r2, ScriptKind::TonV3r2] {
        adapter.set_watch_set(&[watched_as(SECRET, script).unwrap()]).await.unwrap();
    }
}
