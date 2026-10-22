use std::collections::HashSet;
use std::ops::RangeInclusive;
use std::sync::Arc;

use async_trait::async_trait;
use jelly_core::{
    ChainAdapter as _, ChainError, CursorScope, CursorStore as _, Deadline, ScanStream,
    WalletEvent,
    test_util::{FakeClock, FakeEventSink, MemoryCursorStore},
};
use jelly_xrp::{Method, XrpAdapter, XrpError, XrpNode};
use parking_lot::Mutex;
use serde_json::{Value, json};

use super::common::{Node, OWNER, transaction, watched};

#[tokio::test]
async fn history_pages_resume_across_adapter_restarts() {
    let node = Node::new();
    for sequence in 1..=4 {
        node.state.lock().pages.push_back(json!({
            "account":OWNER,"validated":true,"ledger_index_min":32570,"ledger_index_max":90_000_000,
            "transactions":[],"marker":{"ledger":80_000_000,"seq":sequence},
        }));
    }
    let adapter = node.adapter().await.unwrap();
    let sink = FakeEventSink::new();
    let progress = adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    assert!(!progress.is_complete());
    assert_eq!(node.cursors.advance_count(), 3);
    drop(adapter);
    let adapter = node.adapter().await.unwrap();
    assert!(adapter.sync(Deadline::NEVER, &sink).await.unwrap().is_complete());
    let history = node.calls_to(Method::AccountTx);
    assert!(history.first().unwrap().get("marker").is_none());
    assert_eq!(history[3]["marker"]["seq"], 3);
}

#[tokio::test]
async fn unvalidated_history_never_advances_the_cursor() {
    let node = Node::new();
    let mut tx = transaction(&"AB".repeat(32), "tesSUCCESS", 10_000_000, 7_999_990);
    tx["validated"] = false.into();
    node.state.lock().pages.push_back(json!({"account":OWNER,"validated":true,
        "ledger_index_min":32570,"ledger_index_max":90_000_000,"transactions":[tx]}));
    let adapter = node.adapter().await.unwrap();
    let error = adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
    assert_eq!(node.cursors.advance_count(), 0);
    assert_eq!(
        node.cursors.load_scan(CursorScope::Address(1), ScanStream::XrpTransactions).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn a_truncated_history_range_cannot_look_complete() {
    let node = Node::new();
    node.state.lock().pages.push_back(json!({"account":OWNER,"validated":true,
        "ledger_index_min":89_000_000,"ledger_index_max":90_000_000,"transactions":[]}));
    let adapter = node.adapter().await.unwrap();
    let error = adapter.sync(Deadline::NEVER, &FakeEventSink::new()).await.unwrap_err();
    assert!(matches!(error, ChainError::Rpc(_)), "{error:?}");
    assert_eq!(node.cursors.advance_count(), 0);
}

#[tokio::test]
async fn balances_are_total_xrp_and_history_uses_actual_balance_changes() {
    let node = Node::new();
    node.state.lock().ledger = 90_000_001;
    let mut tx = transaction(&"CD".repeat(32), "tesSUCCESS", 10_000_000, 9_999_980);
    tx["Amount"] = "1000000000000".into();
    tx["Flags"] = 0x0002_0000.into();
    tx["meta"]["delivered_amount"] = "10".into();
    node.state.lock().pages.push_back(json!({"account":OWNER,"validated":true,
        "ledger_index_min":32570,"ledger_index_max":90_000_001,"transactions":[tx]}));
    let adapter = node.adapter().await.unwrap();
    let sink = FakeEventSink::new();
    adapter.sync(Deadline::NEVER, &sink).await.unwrap();
    let events = sink.drain();
    assert!(events.iter().any(|event| matches!(event, WalletEvent::BalanceChanged(balance) if balance.balance.base_units() == 10_000_000)));
    assert!(events.iter().any(
        |event| matches!(event, WalletEvent::TxDetected(record) if record.amount.base_units() == 10)
    ));
}

const TIP: u64 = 90_000_000;
const HISTORY_LEDGERS: RangeInclusive<u64> = 50_000_001..=50_000_602;

/// A node whose first pass is answered by rippled and every later pass by Clio.
///
/// The pool's default endpoints are both kinds. The account has one transaction in each
/// history ledger. For the same position, rippled's `account_tx` marker names the next row
/// it has not yet returned, and Clio's names the last row it returned, as both answer on
/// mainnet.
#[derive(Debug, Default)]
struct SwitchingServers {
    passes: Mutex<u32>,
}

impl SwitchingServers {
    fn history_page(&self, params: &Value) -> Result<Value, XrpError> {
        let is_clio = *self.passes.lock() > 1;
        let number =
            |name: &str| params.get(name).and_then(Value::as_u64).ok_or(XrpError::Protocol);
        let first = number("ledger_index_min")?;
        let last = number("ledger_index_max")?;
        let limit = usize::try_from(number("limit")?).map_err(|_| XrpError::Protocol)?;
        let ledgers: Vec<u64> =
            HISTORY_LEDGERS.filter(|ledger| (first..=last).contains(ledger)).collect();
        let start = match params.get("marker") {
            Some(marker) => {
                let resume =
                    marker.get("ledger").and_then(Value::as_u64).ok_or(XrpError::Protocol)?;
                ledgers
                    .iter()
                    .position(|ledger| if is_clio { *ledger > resume } else { *ledger >= resume })
                    .unwrap_or(ledgers.len())
            }
            None => 0,
        };
        let page: Vec<u64> = ledgers.iter().skip(start).take(limit).copied().collect();
        let mut transactions = Vec::with_capacity(page.len());
        for ledger in &page {
            let mut row =
                transaction(&format!("{ledger:064X}"), "tesSUCCESS", 10_000_000, 7_999_990);
            row.as_object_mut()
                .ok_or(XrpError::Protocol)?
                .insert(String::from("ledger_index"), (*ledger).into());
            transactions.push(row);
        }
        let mut result = json!({"account":OWNER,"validated":true,"ledger_index_min":first,
            "ledger_index_max":last,"transactions":transactions});
        if let Some(unreturned) = ledgers.iter().skip(start).nth(limit) {
            let resume = if is_clio { page.last().ok_or(XrpError::Protocol)? } else { unreturned };
            result
                .as_object_mut()
                .ok_or(XrpError::Protocol)?
                .insert(String::from("marker"), json!({"ledger":resume,"seq":0}));
        }
        Ok(result)
    }
}

#[async_trait]
impl XrpNode for SwitchingServers {
    async fn call(&self, method: Method, params: Value) -> Result<Value, XrpError> {
        match method {
            Method::ServerState => {
                let mut passes = self.passes.lock();
                *passes = passes.saturating_add(1);
                Ok(json!({"state":{"validated_ledger":{"seq":TIP,"hash":"AB".repeat(32),
                    "reserve_base":1_000_000,"reserve_inc":200_000,"close_time":0}}}))
            }
            Method::AccountInfo => Ok(json!({"validated":true,"ledger_index":TIP,"account_data":{
                "Account":OWNER,"Balance":"10000000","Sequence":7,"OwnerCount":0,"Flags":0}})),
            Method::AccountTx => self.history_page(&params),
            Method::ServerInfo
            | Method::AccountObjects
            | Method::DepositAuthorized
            | Method::Fee
            | Method::Tx
            | Method::Submit => Err(XrpError::Protocol),
        }
    }

    async fn broadcast(&self, _blob: &str) -> Result<Value, XrpError> {
        Err(XrpError::Protocol)
    }
}

#[tokio::test]
async fn a_history_marker_resumed_on_another_server_skips_no_transaction() {
    let adapter = XrpAdapter::new(
        Arc::new(SwitchingServers::default()),
        Arc::new(MemoryCursorStore::new()),
        Arc::new(FakeClock::default()),
    );
    adapter.set_watch_set(&[watched()]).await.unwrap();
    let sink = FakeEventSink::new();
    for _ in 0..10 {
        if adapter.sync(Deadline::NEVER, &sink).await.unwrap().is_complete() {
            break;
        }
    }
    let detected: HashSet<String> = sink
        .drain()
        .into_iter()
        .filter_map(|event| {
            if let WalletEvent::TxDetected(record) = event {
                Some(record.txid.value().to_owned())
            } else {
                None
            }
        })
        .collect();
    let missing: Vec<u64> =
        HISTORY_LEDGERS.filter(|ledger| !detected.contains(&format!("{ledger:064X}"))).collect();
    assert!(
        missing.is_empty(),
        "expected every transaction in the account's history to be detected, but the ones in \
         ledgers {missing:?} were skipped when a marker stored by one server was resumed on \
         another"
    );
}
