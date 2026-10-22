use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::{
    ChainAdapter as _, ChainSyncProgress, Clock as _, Deadline, TxDirection, TxOutcome,
    WalletEvent,
    test_util::{FakeClock, FakeEventSink, MemoryCursorStore},
};
use jelly_zcash::{Route, ZcashAdapter, ZcashError, ZcashNode};
use serde_json::{Value, json};
use tokio::time::sleep;
use zcash_transparent::bundle::OutPoint;

use super::common::{
    HEIGHT, Node, OWNER, RECIPIENT, consolidation, document, pay_to_owner_pubkey, transaction,
    watched,
};

/// `PooledNode` waits this long before every read, so no pass can read faster.
const READ_PACE: Duration = Duration::from_secs(2);

#[derive(Debug)]
struct PacedNode {
    node: Arc<Node>,
}

#[async_trait]
impl ZcashNode for PacedNode {
    async fn get(&self, route: Route<'_>) -> Result<Value, ZcashError> {
        sleep(READ_PACE).await;
        self.node.clock.advance(READ_PACE);
        self.node.get(route).await
    }

    async fn broadcast(&self, raw_hex: &str) -> Result<String, ZcashError> {
        self.node.broadcast(raw_hex).await
    }
}

#[tokio::test]
async fn balances_history_confirmations_and_restart_are_consistent() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let sink = FakeEventSink::new();
    assert!(adapter.sync(Deadline::NEVER, &sink).await.unwrap().is_complete());
    let events = sink.drain();
    assert!(events.iter().any(|event| matches!(event, WalletEvent::BalanceChanged(balance) if balance.balance.base_units() == 1_000_000)));
    assert!(events.iter().any(|event| matches!(event, WalletEvent::TxDetected(record) if record.direction == TxDirection::Incoming && record.amount.base_units() == 1_000_000 && record.fee.unwrap().base_units() == 10_000 && record.confirmations == 6)));
    drop(adapter);
    let adapter = node.adapter().await.unwrap();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    assert_eq!(node.cursors.advance_count(), 2);
}

#[tokio::test]
async fn pagination_is_complete_and_reorgs_clear_inclusions() {
    let node = Node::new().unwrap();
    let reward = transaction(OutPoint::NULL, OWNER, 50_000).unwrap();
    let txid = reward.txid().to_string();
    {
        let mut state = node.state.lock();
        state.transactions.insert(txid.clone(), document(&reward, Some(HEIGHT)).unwrap());
        state.history.push(txid.clone());
        state.page_size = 1;
    }
    let adapter = node.adapter().await.unwrap();
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    assert!(node.state.lock().history_pages.contains(&2));
    sink.drain();
    {
        let mut state = node.state.lock();
        state.hash = "cd".repeat(32);
        state.history.retain(|id| id != &txid);
        let funding_id = node.funding.txid().to_string();
        state.transactions.insert(funding_id, document(&node.funding, None).unwrap());
        state.balance = String::from("0");
        state.unconfirmed_balance = String::from("1000000");
    }
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    let events = sink.drain();
    assert!(events.iter().any(|event| matches!(event, WalletEvent::TxConfirmed(record) if record.txid.value() == txid && record.confirmations == 0)));
    assert!(events.iter().any(|event| matches!(event, WalletEvent::TxDetected(record) if record.txid.value() == node.funding.txid().to_string() && record.height.is_none() && record.outcome == TxOutcome::Success)));
}

#[tokio::test]
async fn large_histories_resume_after_a_partial_pass_and_restart() {
    let node = Node::new().unwrap();
    for value_zat in 10_000..10_102 {
        let reward = transaction(OutPoint::NULL, OWNER, value_zat).unwrap();
        let txid = reward.txid().to_string();
        let mut state = node.state.lock();
        state
            .transactions
            .insert(txid.clone(), document(&reward, Some(HEIGHT.saturating_sub(100))).unwrap());
        state.history.push(txid);
    }
    let adapter = node.adapter().await.unwrap();
    let sink = FakeEventSink::new();
    assert!(!adapter.sync(Deadline::NEVER, &sink).await.unwrap().is_complete());
    assert_eq!(node.cursors.advance_count(), 1);
    drop(adapter);
    let adapter = node.adapter().await.unwrap();
    assert!(adapter.sync(Deadline::NEVER, &sink).await.unwrap().is_complete());
    assert_eq!(node.cursors.advance_count(), 2);
}

#[tokio::test]
async fn a_partial_pass_after_a_reorg_reports_no_unvisited_history_as_dropped() {
    let node = Node::new().unwrap();
    for value_zat in 10_000..10_102 {
        let reward = transaction(OutPoint::NULL, OWNER, value_zat).unwrap();
        let txid = reward.txid().to_string();
        let mut state = node.state.lock();
        state
            .transactions
            .insert(txid.clone(), document(&reward, Some(HEIGHT.saturating_sub(100))).unwrap());
        state.history.push(txid);
    }
    let adapter = node.adapter().await.unwrap();
    let sink = FakeEventSink::new();
    assert!(!adapter.sync(Deadline::NEVER, &sink).await.unwrap().is_complete());
    assert!(adapter.sync(Deadline::NEVER, &sink).await.unwrap().is_complete());
    sink.drain();
    node.state.lock().hash = "cd".repeat(32);
    assert!(!adapter.sync(Deadline::NEVER, &sink).await.unwrap().is_complete());
    let history = node.state.lock().history.clone();
    let dropped: Vec<String> = sink
        .drain()
        .into_iter()
        .filter_map(|event| {
            if let WalletEvent::TxConfirmed(confirmation) = event
                && confirmation.confirmations == 0
            {
                Some(confirmation.txid.value().to_owned())
            } else {
                None
            }
        })
        .filter(|txid| history.contains(txid))
        .collect();
    assert!(
        dropped.is_empty(),
        "expected no dropped inclusion for a transaction the pass never revisited and that is \
         still confirmed at height {}, but {} were reported dropped, such as {:?}",
        HEIGHT.saturating_sub(100),
        dropped.len(),
        dropped.first()
    );
}

#[tokio::test]
async fn a_pay_to_pubkey_payment_in_the_address_history_syncs() {
    let node = Node::new().unwrap();
    let parent = transaction(OutPoint::NULL, RECIPIENT, 20_000).unwrap();
    let gift = pay_to_owner_pubkey(OutPoint::new(*parent.txid().as_ref(), 0), 10_000).unwrap();
    let txid = gift.txid().to_string();
    {
        let mut state = node.state.lock();
        state.transactions.insert(parent.txid().to_string(), document(&parent, Some(1)).unwrap());
        state.transactions.insert(txid.clone(), document(&gift, Some(HEIGHT)).unwrap());
        state.history.insert(0, txid);
        state.balance = String::from("1010000");
    }
    let adapter = node.adapter().await.unwrap();
    let sink = FakeEventSink::new();
    let progress = adapter.sync(Deadline::NEVER, &sink).await;
    assert!(
        progress.is_ok(),
        "expected a sync pass to succeed when Blockbook lists a pay-to-pubkey payment to the \
         address's key in its history, got {progress:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn a_transaction_with_more_inputs_than_one_pass_can_fetch_is_finished_by_later_passes() {
    let node = Node::new().unwrap();
    let mut inputs = Vec::new();
    {
        let mut state = node.state.lock();
        for index in 0..300 {
            let value_zat = u64::from(index).saturating_add(10_000);
            let parent = transaction(OutPoint::new([1; 32], index), OWNER, value_zat).unwrap();
            inputs.push(OutPoint::new(*parent.txid().as_ref(), 0));
            state.transactions.insert(
                parent.txid().to_string(),
                document(&parent, Some(HEIGHT.saturating_sub(20))).unwrap(),
            );
        }
        let merged = consolidation(inputs, OWNER, 100_000).unwrap();
        let txid = merged.txid().to_string();
        state
            .transactions
            .insert(txid.clone(), document(&merged, Some(HEIGHT.saturating_sub(10))).unwrap());
        state.history.insert(0, txid);
    }
    let adapter = ZcashAdapter::new(
        Arc::new(PacedNode { node: Arc::clone(&node) }),
        Arc::<MemoryCursorStore>::clone(&node.cursors),
        Arc::<FakeClock>::clone(&node.clock),
    );
    adapter.set_watch_set(&[watched()]).await.unwrap();
    let sink = FakeEventSink::new();
    let mut outcomes = Vec::new();
    for _ in 0..10 {
        let deadline = node.clock.deadline_in(Duration::from_secs(120));
        let outcome = adapter.sync(deadline, &sink).await.map(ChainSyncProgress::is_complete);
        let is_complete = matches!(outcome, Ok(true));
        outcomes.push(outcome);
        if is_complete {
            break;
        }
    }
    assert!(
        matches!(outcomes.last(), Some(Ok(true))),
        "expected repeated 120 second passes to finish the history of an address holding one \
         300 input transaction, but none did: {outcomes:?}"
    );
}

#[tokio::test]
async fn truncated_or_forged_history_never_advances_the_cursor() {
    let node = Node::new().unwrap();
    let adapter = node.adapter().await.unwrap();
    let sink = FakeEventSink::new();
    node.state.lock().missing_history_count = 1;
    adapter.sync(Deadline::NEVER, &sink).await.unwrap_err();
    assert_eq!(node.cursors.advance_count(), 0);
    assert!(sink.is_empty());
    node.state.lock().missing_history_count = 0;
    let txid = node.funding.txid().to_string();
    node.state.lock().transactions.get_mut(&txid).unwrap()["hex"] = json!("00");
    adapter.sync(Deadline::NEVER, &sink).await.unwrap_err();
    assert_eq!(node.cursors.advance_count(), 0);
}
