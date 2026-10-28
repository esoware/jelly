use std::collections::HashSet;
use std::io;
use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, B256, U256};
use async_trait::async_trait;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainId, Clock, CursorScope, CursorStore as _, ScanStream,
    TokenId, TxDetection, TxDirection, TxOutcome, WalletEvent,
    test_util::{FakeClock, FakeEventSink, MemoryCursorStore},
};
use jelly_evm::{
    EtherscanHistory, EvmAdapter, EvmConfig, EvmError, HistoryApi, HistoryPage, InternalRow,
    Method, NativeRow, TokenRow, test_util::FakeHistory,
};
use parking_lot::Mutex;
use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::common::{
    self, AVALANCHE, Answer, BASE, BSC, ETH, ETHEREUM, FakeNode, Fallible, Harness, L1_FEE_WEI,
    OWNER, PEER, SHEET_ENTRIES, Setup, TIP, TIP_HEX, aggregate, fixture, http_pool, quantity,
    sheet, sheet_index, sheets_on, watched,
};

#[test]
fn a_history_service_with_no_key_is_refused_at_construction() {
    let pool = http_pool(ChainId::Ethereum, &["https://api.example/v2/api"]).unwrap();
    for blank in ["", "   "] {
        assert!(matches!(
            EtherscanHistory::new(Arc::clone(&pool), blank, ETHEREUM),
            Err(EvmError::HistoryKeyMissing)
        ));
    }
    let good = EtherscanHistory::new(pool, " a-key ", ETHEREUM).unwrap();
    let rendered = format!("{good:?}");
    assert!(!rendered.contains("a-key"), "the key is never printed: {rendered}");
    assert!(rendered.contains("chain_id: 1"), "{rendered}");
}

/// A harness whose history service serves the recorded pages.
async fn indexed() -> Fallible<Harness> {
    let history = FakeHistory::new();
    history.serve(&fixture("etherscan.json", "txlist")?, &fixture("etherscan.json", "tokentx")?)?;
    let harness = Harness::new(Setup::with_history(history))?;
    harness.watch_owner().await?;
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    let mut balances = sheet(5 * ETH);
    *balances.get_mut(sheet_index(ETHEREUM, TokenId::UsdtErc20)?).ok_or("a sheet entry")? =
        Some(2_500_000);
    harness.node.always(Method::Call, Answer::ok(&aggregate(&balances)));
    Ok(harness)
}

#[tokio::test]
async fn an_indexed_pass_records_native_and_token_history_and_asks_for_no_logs() {
    let harness = indexed().await.unwrap();
    harness.sync().await.unwrap();

    assert!(
        harness.node.calls_to(Method::GetLogs).is_empty(),
        "an indexer covers what a log walk would find, and native transfers besides"
    );
    let records = harness.records();
    assert_eq!(records.len(), 5, "three native rows and the two token rows that are ours");

    let incoming = records
        .iter()
        .find(|record| {
            record.asset == AssetId::Native(ChainId::Ethereum)
                && record.direction == TxDirection::Incoming
        })
        .expect("the incoming coin transfer");
    assert_eq!(incoming.amount.base_units(), ETH);
    assert_eq!(incoming.outcome, TxOutcome::Success);
    assert_eq!(incoming.height, Some(18_000_000));
    assert_eq!(incoming.timestamp, Some(1_692_000_000));
    assert_eq!(incoming.fee, None, "the sender paid, not us");
    assert_eq!(incoming.detection, TxDetection::Full);

    let failed =
        records.iter().find(|record| record.outcome == TxOutcome::Failed).expect("the failed row");
    assert_eq!(failed.amount.base_units(), 0, "a failed transaction moved nothing");
    assert_eq!(
        failed.fee.map(Amount::base_units),
        Some(21_000 * 20_000_000_000),
        "and still paid for its gas"
    );
    assert_eq!(failed.direction, TxDirection::Outgoing);
    assert_eq!(failed.counterparty.as_deref(), Some(PEER));

    let token = records
        .iter()
        .find(|record| record.asset == AssetId::Token(TokenId::UsdtErc20))
        .expect("a token row");
    assert_eq!(token.amount.base_units(), 2_500_000);
}

#[tokio::test]
async fn a_complete_indexed_read_closes_both_frontiers() {
    let harness = indexed().await.unwrap();
    harness.sync().await.unwrap();
    let stored = harness
        .cursors
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await
        .unwrap()
        .expect("a cursor row");
    let cursor: Value = stored.with_bytes(|bytes| serde_json::from_slice(bytes)).unwrap();
    assert_eq!(
        cursor.get("forwardTo").and_then(Value::as_u64),
        Some(TIP - ETHEREUM.indexer_lag_blocks()),
        "the service may not have indexed the newest blocks yet, so they are not claimed"
    );
    assert_eq!(
        cursor.get("backfillFrom").and_then(Value::as_u64),
        Some(4_281_611),
        "an indexer answers the whole history at once, so it is complete"
    );

    let reads = harness.history.as_ref().expect("a history service").reads();
    assert_eq!(reads.len(), 2, "one native read and one token read");
    assert!(
        reads.iter().all(|(_, start)| *start == 4_281_611),
        "the first read starts at the floor: {reads:?}"
    );
}

#[tokio::test]
async fn a_truncated_indexed_read_resumes_where_its_shortest_list_stopped() {
    let history = FakeHistory::new();
    history
        .serve(
            &fixture("etherscan.json", "txlist").unwrap(),
            &fixture("etherscan.json", "tokentx").unwrap(),
        )
        .unwrap();
    history.truncate();
    let harness = Harness::new(Setup::with_history(history)).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));

    harness.sync().await.unwrap();
    let stored = harness
        .cursors
        .load_scan(CursorScope::Address(7), ScanStream::EvmLogScan)
        .await
        .unwrap()
        .expect("a cursor row");
    let cursor: Value = stored.with_bytes(|bytes| serde_json::from_slice(bytes)).unwrap();
    // The internal list is untruncated and empty, so the native and token lists
    // decide. Native rows reach 18_000_005 and token rows only 18_000_002, and the
    // pass claims one block below the lower of the two, because that block may be
    // only partly returned.
    assert_eq!(cursor.get("forwardTo").and_then(Value::as_u64), Some(18_000_001));
    assert_eq!(
        cursor.get("backfillFrom").and_then(Value::as_u64),
        Some(4_281_611),
        "the read started at the floor, so everything below what it returned is covered"
    );

    harness.drain();
    let before = harness.history.as_ref().expect("a history service").reads().len();
    harness.sync().await.unwrap();
    let after = harness.history.as_ref().expect("a history service").reads();
    let resume = 18_000_001 - ETHEREUM.rescan_overlap_blocks();
    assert!(
        after.iter().skip(before).all(|(_, start)| *start == resume),
        "the next read continues from the frontier, less the reorganisation overlap: {after:?}"
    );
}

#[tokio::test]
async fn an_indexed_pass_that_reported_a_native_row_makes_no_balance_delta() {
    let harness = indexed().await.unwrap();
    let mut balances = sheet(5 * ETH);
    balances[sheet_index(ETHEREUM, TokenId::UsdtErc20).unwrap()] = Some(2_500_000);
    harness.node.on(Method::Call, Answer::ok(&aggregate(&balances)));
    harness.sync().await.unwrap();
    harness.drain();

    // The balance moves and the indexer explains it.
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheet(6 * ETH))));
    harness.sync().await.unwrap();
    let records = harness.records();
    assert!(
        records.iter().all(|record| record.detection == TxDetection::Full),
        "an indexer accounts for a native change, so nothing is guessed at: {records:?}"
    );
}

#[tokio::test]
async fn a_history_service_that_fails_leaves_the_balances_standing() {
    let harness = indexed().await.unwrap();
    harness.history.as_ref().expect("a history service").fail();
    let progress = harness.sync().await.unwrap();
    let events = harness.drain();
    let balances =
        events.iter().filter(|event| matches!(event, WalletEvent::BalanceChanged(_))).count();
    assert_eq!(balances, SHEET_ENTRIES);
    assert!(!events.iter().any(|event| matches!(event, WalletEvent::TxDetected(_))));
    assert!(progress.synced_height().is_none(), "history nobody read is not synced");
}

#[tokio::test]
async fn a_second_indexed_pass_resumes_at_the_frontier_rather_than_the_floor() {
    let harness = indexed().await.unwrap();
    harness.sync().await.unwrap();
    let history = harness.history.as_ref().expect("a history service");
    let before = history.reads().len();

    harness.clock.advance(EvmConfig::default().history_sweep);
    harness.sync().await.unwrap();
    let after = history.reads();
    assert_eq!(after.len(), before.saturating_add(2));
    // The frontier stops short of the tip by what the service may trail the node
    // by, and the resume point sits one reorg depth below it, because rows in
    // those blocks can still change.
    let resume = TIP - ETHEREUM.indexer_lag_blocks() - ETHEREUM.rescan_overlap_blocks();
    assert!(
        after.iter().skip(before).all(|(_, start)| *start == resume),
        "a complete history resumes just below the frontier: {after:?}"
    );
}

/// BSC was at block 75,140,593 when Fermi moved it to 0.45-second blocks on 14
/// January 2026, so it passed block 99,999,999 in May 2026.
const BSC_HEIGHT_IN_2026: u64 = 120_000_000;

const EMPTY_PAGE: &str = r#"{"status":"0","message":"No transactions found","result":[]}"#;

/// A history service on a loopback port, which answers each request with the body
/// for the `page` it names, or with an empty page.
struct LoopbackService {
    url: String,
    stop: oneshot::Sender<()>,
    server: JoinSet<io::Result<Vec<String>>>,
}

impl LoopbackService {
    /// Gives up after twenty seconds without a caller.
    async fn start(pages: Vec<String>) -> Fallible<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let url = format!("http://{}/api", listener.local_addr()?);
        let (stop, mut receiver) = oneshot::channel();
        let mut server = JoinSet::new();
        server.spawn(async move {
            let mut lines = Vec::new();
            loop {
                let (mut stream, _) = tokio::select! {
                    _ = &mut receiver => return Ok(lines),
                    accepted = timeout(Duration::from_secs(20), listener.accept()) => accepted??,
                };
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    timeout(Duration::from_secs(10), stream.read_exact(&mut byte)).await??;
                    head.extend_from_slice(&byte);
                }
                let line =
                    String::from_utf8_lossy(&head).lines().next().unwrap_or_default().to_owned();
                let body = page_of(&line)
                    .checked_sub(1)
                    .and_then(|index| pages.get(usize::try_from(index).ok()?))
                    .map_or(EMPTY_PAGE, String::as_str);
                lines.push(line);
                let reply = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                     connection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).await?;
                stream.shutdown().await?;
            }
        });
        Ok(Self { url, stop, server })
    }

    /// Stops answering and hands back every request line, in order.
    async fn finish(mut self) -> Fallible<Vec<String>> {
        let _ = self.stop.send(());
        Ok(self.server.join_next().await.ok_or("the service task is missing")???)
    }
}

/// The `page` a request line asks for, or zero when it names none.
fn page_of(line: &str) -> u32 {
    line.split(['?', '&', ' '])
        .find_map(|pair| pair.strip_prefix("page="))
        .and_then(|text| text.parse::<u32>().ok())
        .unwrap_or(0)
}

/// One `tokentx` row of `value` at `block`.
fn token_row(block: u64, hash: B256, contract: &str, value: &str) -> Value {
    serde_json::json!({
        "blockNumber": block.to_string(),
        "timeStamp": "1692000000",
        "hash": hash.to_string(),
        "from": PEER,
        "to": OWNER,
        "contractAddress": contract,
        "value": value,
        "tokenDecimal": "6",
        "gasUsed": "63000",
        "gasPrice": "20000000000",
    })
}

/// An address that has received a thousand token transfers, one of them from a
/// spam token whose value no 128-bit integer holds, which airdrop spam often uses.
#[tokio::test]
async fn a_full_history_page_with_an_unreadable_row_does_not_end_the_walk() {
    const SPAM: &str = "0x00000000000000000000000000000000DeaDBeef";
    let mut first = Vec::with_capacity(1_000);
    for index in 0..1_000u64 {
        let mut hash = [0u8; 32];
        hash[24..].copy_from_slice(&index.to_be_bytes());
        let block = 18_000_000 + index;
        first.push(if index == 500 {
            token_row(block, B256::from(hash), SPAM, &U256::MAX.to_string())
        } else {
            token_row(block, B256::from(hash), common::USDT, "1000000")
        });
    }
    let later = B256::repeat_byte(0x77);
    let pages = vec![
        serde_json::json!({"status": "1", "message": "OK", "result": first}).to_string(),
        serde_json::json!({
            "status": "1",
            "message": "OK",
            "result": [token_row(18_100_000, later, common::USDT, "2500000")],
        })
        .to_string(),
    ];

    let service = LoopbackService::start(pages).await.unwrap();
    let pool = http_pool(ChainId::Ethereum, &[&service.url]).unwrap();
    let history = EtherscanHistory::new(pool, "a-key", ETHEREUM).unwrap();
    let owner: Address = OWNER.parse().unwrap();

    let read = timeout(Duration::from_secs(20), history.tokens(owner, 0)).await;
    let asked: Vec<u32> =
        service.finish().await.unwrap().iter().map(|line| page_of(line)).collect();
    let page = read.expect("the read finished inside its budget").expect("the service answered");

    let reaches_page_two = page.rows().iter().any(|row| row.hash == later);
    assert!(
        reaches_page_two || page.is_truncated(),
        "the first page held a full thousand rows, so the history goes on, yet the read stopped \
         after pages {asked:?} with {} rows, no truncation and nothing from block 18100000, so \
         the cursor moves past every transfer it never fetched",
        page.rows().len()
    );
}

#[tokio::test]
async fn a_bsc_history_read_is_not_capped_below_the_current_height() {
    let service = LoopbackService::start(Vec::new()).await.unwrap();
    let pool = http_pool(ChainId::Bsc, &[&service.url]).unwrap();
    let history = EtherscanHistory::new(pool, "a-key", BSC).unwrap();
    let owner: Address = OWNER.parse().unwrap();

    timeout(Duration::from_secs(20), history.native(owner, BSC_HEIGHT_IN_2026))
        .await
        .expect("the read finished inside its budget")
        .expect("the service answered");
    let lines = service.finish().await.unwrap();
    let line = lines.first().expect("the service was asked");

    let end = line.split(['?', '&', ' ']).find_map(|pair| pair.strip_prefix("endblock="));
    let is_capped_below_the_tip = end
        .is_some_and(|text| matches!(text.parse::<u64>(), Ok(block) if block < BSC_HEIGHT_IN_2026));
    assert!(!is_capped_below_the_tip, "the read ends below where the chain already is: {line}");
}

/// Answers native rows at or after the asked block and no later than the block it
/// has indexed to, as the service does.
#[derive(Debug)]
struct Indexer {
    head: Mutex<u64>,
    rows: Vec<NativeRow>,
}

#[async_trait]
impl HistoryApi for Indexer {
    async fn native(
        &self,
        _address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<NativeRow>, EvmError> {
        let head = *self.head.lock();
        let rows = self
            .rows
            .iter()
            .filter(|row| row.block_number >= start_block && row.block_number <= head)
            .cloned()
            .collect();
        Ok(HistoryPage::of(rows, false))
    }

    async fn tokens(
        &self,
        _address: Address,
        _start_block: u64,
    ) -> Result<HistoryPage<TokenRow>, EvmError> {
        Ok(HistoryPage::of(Vec::new(), false))
    }

    async fn internal(
        &self,
        _address: Address,
        _start_block: u64,
    ) -> Result<HistoryPage<InternalRow>, EvmError> {
        Ok(HistoryPage::of(Vec::new(), false))
    }
}

#[tokio::test]
async fn an_indexer_a_few_seconds_behind_the_node_loses_no_rows() {
    let landed_at = TIP - 18;
    let row = NativeRow {
        block_number: landed_at,
        timestamp: 1_760_000_000,
        hash: B256::repeat_byte(0x5a),
        from: PEER.parse().unwrap(),
        to: Some(OWNER.parse().unwrap()),
        value: ETH,
        gas_used: Some(21_000),
        gas_price: Some(100_000_000),
        is_failed: false,
    };
    let indexer = Arc::new(Indexer { head: Mutex::new(TIP - 20), rows: vec![row] });
    let clock = Arc::new(FakeClock::default());
    let as_clock: Arc<dyn Clock> = Arc::<FakeClock>::clone(&clock);
    let node = FakeNode::new();
    let api: Arc<dyn HistoryApi> = Arc::<Indexer>::clone(&indexer);
    let adapter = EvmAdapter::builder(
        BSC,
        Arc::<FakeNode>::clone(&node),
        Arc::new(MemoryCursorStore::new()),
        as_clock,
    )
    .history(api)
    .build()
    .unwrap();
    let events = FakeEventSink::new();
    adapter.set_watch_set(&[watched(7, ChainId::Bsc, OWNER)]).await.unwrap();
    node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    node.always(Method::Call, Answer::ok(&aggregate(&sheets_on(BSC, 1, 0))));
    node.always(Method::GetTransactionCount, Answer::ok(&quantity(0)));

    // Nine seconds of BSC blocks behind the node, which the service often is.
    adapter.sync(clock.deadline_in(Duration::from_secs(60)), &events).await.unwrap();

    // Nothing in the balances says so, so the sweep is the read that finds it.
    *indexer.head.lock() = TIP;
    node.always(Method::BlockNumber, Answer::ok(&quantity(u128::from(TIP + 2))));
    clock.advance(EvmConfig::default().history_sweep);
    adapter.sync(clock.deadline_in(Duration::from_secs(60)), &events).await.unwrap();

    let hash = B256::repeat_byte(0x5a).to_string();
    let recorded = events.drain().into_iter().any(
        |event| matches!(event, WalletEvent::TxDetected(ref record) if record.txid.value() == hash),
    );
    assert!(recorded, "a payment in block {landed_at} was never read once the service had it");
}

#[tokio::test]
async fn each_watched_address_gets_its_own_native_and_token_reads() {
    let history = FakeHistory::new();
    history
        .serve(
            &fixture("etherscan.json", "empty").unwrap(),
            &fixture("etherscan.json", "empty").unwrap(),
        )
        .unwrap();
    let harness = Harness::new(Setup::with_history(history)).unwrap();
    harness
        .watch(&[
            watched(7, ChainId::Ethereum, OWNER),
            watched(8, ChainId::Ethereum, common::SECOND),
        ])
        .await
        .unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheets_on(ETHEREUM, 2, 0))));

    harness.sync().await.unwrap();
    let reads = harness.history.as_ref().expect("a history service").reads();
    assert_eq!(reads.len(), 4, "two addresses, two actions each");
}

/// A harness on Ethereum whose history service has nothing to say yet, with the
/// address holding `balance` wei.
async fn quiet_indexer(balance: u128) -> Fallible<(Harness, Arc<FakeHistory>)> {
    let history = FakeHistory::new();
    let empty = fixture("etherscan.json", "empty")?;
    history.serve(&empty, &empty)?;
    let harness = Harness::new(Setup::with_history(Arc::clone(&history)))?;
    harness.watch_owner().await?;
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(balance))));
    Ok((harness, history))
}

/// A fresh adapter over the harness's node, cursors, clock, and `history`, as the
/// next session builds it.
fn restart(harness: &Harness, history: &Arc<FakeHistory>) -> Fallible<EvmAdapter> {
    Ok(EvmAdapter::builder(
        ETHEREUM,
        Arc::<FakeNode>::clone(&harness.node),
        Arc::<MemoryCursorStore>::clone(&harness.cursors),
        Arc::<FakeClock>::clone(&harness.clock),
    )
    .history(Arc::<FakeHistory>::clone(history))
    .build()?)
}

/// A `txlist` page holding one coin paid to the owner in `block`.
fn payment_in(block: u64) -> String {
    serde_json::json!({
        "status": "1",
        "message": "OK",
        "result": [{
            "blockNumber": block.to_string(),
            "timeStamp": "1692001200",
            "hash": format!("0x{}", "5b".repeat(32)),
            "from": PEER,
            "to": OWNER,
            "value": ETH.to_string(),
            "gasUsed": "21000",
            "gasPrice": "20000000000",
            "isError": "0",
        }],
    })
    .to_string()
}

#[tokio::test]
async fn coin_a_contract_pays_back_or_pays_out_is_netted_into_one_record_per_transaction() {
    let history = FakeHistory::new();
    history
        .serve(
            &fixture("etherscan.json", "txlist_refund").unwrap(),
            &fixture("etherscan.json", "empty").unwrap(),
        )
        .unwrap();
    history.serve_internal(&fixture("etherscan.json", "txlistinternal").unwrap()).unwrap();
    let harness = Harness::new(Setup::with_history(history)).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));
    harness.sync().await.unwrap();

    let records = harness.records();
    let mut identities = HashSet::new();
    for record in &records {
        assert!(
            identities.insert((record.address_id, record.asset, record.txid.clone())),
            "the store keeps one row per address, asset and txid, so this one overwrites the \
             other: {records:?}"
        );
    }
    assert_eq!(records.len(), 2, "the refunded send and the payout, and nothing else: {records:?}");

    let refunded = records
        .iter()
        .find(|record| record.txid.value().starts_with("0x55aa"))
        .expect("the send the router refunded");
    assert_eq!(refunded.direction, TxDirection::Outgoing);
    assert_eq!(
        refunded.amount.base_units(),
        700_000_000_000_000_000,
        "one coin out and three tenths back"
    );
    assert_eq!(
        refunded.fee.map(Amount::base_units),
        Some(120_000 * 20_000_000_000),
        "the sender's fee, counted once"
    );
    assert_eq!(
        refunded.counterparty.as_deref(),
        Some("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D")
    );

    let payout = records
        .iter()
        .find(|record| record.txid.value().starts_with("0x66bb"))
        .expect("the payout");
    assert_eq!(payout.direction, TxDirection::Incoming);
    assert_eq!(payout.amount.base_units(), 500_000_000_000_000_000);
    assert_eq!(payout.fee, None, "the contract's caller paid for the gas, not us");
    assert_eq!(payout.detection, TxDetection::Full);
    assert_eq!(payout.height, Some(18_000_030));
}

#[tokio::test]
async fn a_change_only_an_internal_call_explains_gets_no_delta_and_an_unexplained_one_does() {
    let (harness, history) = quiet_indexer(5 * ETH).await.unwrap();
    harness.sync().await.unwrap();
    harness.drain();

    // Contracts pay the address, and only the internal list says so.
    history.serve_internal(&fixture("etherscan.json", "txlistinternal").unwrap()).unwrap();
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(5_800_000_000_000_000_000))));
    harness.sync().await.unwrap();
    let explained = harness.records();
    assert!(!explained.is_empty());
    assert!(
        explained.iter().all(|record| record.detection == TxDetection::Full),
        "an internal transfer accounts for the change, so nothing is guessed at: {explained:?}"
    );

    // Nothing the service reports explains the next change, which waits until
    // the service could have indexed the block that showed it.
    history.serve_internal(&fixture("etherscan.json", "empty").unwrap()).unwrap();
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(6_800_000_000_000_000_000))));
    harness.sync().await.unwrap();
    assert!(harness.records().is_empty(), "the service may still report it");

    let caught_up = TIP + ETHEREUM.indexer_lag_blocks();
    harness.node.always(Method::BlockNumber, Answer::ok(&quantity(u128::from(caught_up))));
    harness.sync().await.unwrap();
    let guessed = harness.records();
    assert_eq!(guessed.len(), 1, "{guessed:?}");
    let delta = guessed.first().expect("the delta");
    assert_eq!(delta.detection, TxDetection::BalanceDelta);
    assert_eq!(delta.direction, TxDirection::Incoming);
    assert_eq!(delta.amount.base_units(), ETH);
}

/// A free Etherscan key allows 100,000 requests a day and a pass runs every
/// block, so a pass that sees no news leaves the service alone.
#[tokio::test]
async fn a_pass_that_sees_no_news_does_not_ask_the_history_service() {
    let (harness, history) = quiet_indexer(5 * ETH).await.unwrap();
    harness.sync().await.unwrap();
    assert_eq!(history.reads().len(), 2, "the first pass reads the transactions and tokens");
    assert_eq!(history.internal_reads().len(), 1, "and the internal calls");

    for _ in 0..3u8 {
        harness.sync().await.unwrap();
    }
    assert_eq!(history.reads().len(), 2, "nothing moved, so nothing was asked");
    assert_eq!(history.internal_reads().len(), 1);
}

#[tokio::test]
async fn a_coin_or_token_balance_change_is_read_in_the_pass_that_sees_it() {
    let mut token_moved = sheet(5 * ETH);
    token_moved[1] = Some(2_500_000);
    for balances in [sheet(6 * ETH), token_moved] {
        let (harness, history) = quiet_indexer(5 * ETH).await.unwrap();
        harness.sync().await.unwrap();
        harness.sync().await.unwrap();
        let before = history.reads().len();

        harness.node.always(Method::Call, Answer::ok(&aggregate(&balances)));
        harness.sync().await.unwrap();
        assert_eq!(history.reads().len(), before.saturating_add(2), "{balances:?}");
    }
}

/// A send of the address's own moves its nonce, which also catches a payment in
/// that exactly cancels what the send cost.
#[tokio::test]
async fn a_nonce_change_is_read_even_when_no_balance_moved() {
    let (harness, history) = quiet_indexer(5 * ETH).await.unwrap();
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(3)));
    harness.sync().await.unwrap();
    harness.sync().await.unwrap();
    let before = history.reads().len();
    assert_eq!(harness.node.calls_to(Method::GetTransactionCount).len(), 2, "one a pass");
    assert!(
        harness.node.calls_to(Method::GetBalance).is_empty(),
        "the nonce rides with the aggregate, so the balances never fall back to a call each"
    );

    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(4)));
    harness.sync().await.unwrap();
    assert_eq!(history.reads().len(), before.saturating_add(2));
}

#[tokio::test]
async fn the_sweep_reads_a_quiet_address_once_its_interval_has_passed() {
    let sweep = EvmConfig::default().history_sweep;
    let (harness, history) = quiet_indexer(5 * ETH).await.unwrap();
    harness.sync().await.unwrap();
    let first = history.reads().len();

    harness.clock.advance(sweep.saturating_sub(Duration::from_secs(1)));
    harness.sync().await.unwrap();
    assert_eq!(history.reads().len(), first, "the sweep is not due yet");

    harness.clock.advance(Duration::from_secs(1));
    harness.sync().await.unwrap();
    assert_eq!(history.reads().len(), first.saturating_add(2), "the sweep reads it");
    assert_eq!(
        history.reads().last().map(|read| read.1),
        Some(TIP - ETHEREUM.indexer_lag_blocks() - ETHEREUM.rescan_overlap_blocks()),
        "from the frontier, so nothing the sweep waited on is skipped"
    );
}

/// An address the adapter has not read since it was built has no sheet to
/// compare, so it is read on its first pass whatever its balances say. After a
/// restart that is every address.
#[tokio::test]
async fn a_cold_address_is_read_on_its_first_pass() {
    let both =
        [watched(7, ChainId::Ethereum, OWNER), watched(8, ChainId::Ethereum, common::SECOND)];
    let (harness, history) = quiet_indexer(0).await.unwrap();
    harness.sync().await.unwrap();

    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheets_on(ETHEREUM, 2, 0))));
    harness.watch(&both).await.unwrap();
    harness.sync().await.unwrap();
    let second: Address = common::SECOND.parse().unwrap();
    let reads = history.reads();
    assert_eq!(reads.iter().filter(|(owner, _)| *owner == second).count(), 2, "{reads:?}");
    assert_eq!(reads.len(), 4, "the address already read had no news");

    let restarted = restart(&harness, &history).unwrap();
    restarted.set_watch_set(&both).await.unwrap();
    restarted.sync(harness.deadline(), &harness.events).await.unwrap();
    assert_eq!(history.reads().len(), 8, "a restart reads every address once");
}

/// A restart forgets the sheet the last pass saw, but the cursor keeps the
/// balance it judged, so a payment made while the wallet was closed is still news.
#[tokio::test]
async fn a_change_made_while_the_wallet_was_closed_is_news_after_a_restart() {
    let (harness, history) = quiet_indexer(5 * ETH).await.unwrap();
    harness.sync().await.unwrap();
    harness.drain();

    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(6 * ETH))));
    let restarted = restart(&harness, &history).unwrap();
    restarted.set_watch_set(&[watched(7, ChainId::Ethereum, OWNER)]).await.unwrap();
    restarted.sync(harness.deadline(), &harness.events).await.unwrap();
    assert!(harness.records().is_empty(), "the service may not have the payment yet");

    let before = history.reads().len();
    restarted.sync(harness.deadline(), &harness.events).await.unwrap();
    assert_eq!(history.reads().len(), before.saturating_add(2), "so it is asked again");
}

/// The node shows a payment before the service has indexed it.
///
/// The change waits rather than becoming a guess, the address is read again on the
/// next pass, and once the service has reported the payment the address goes quiet.
#[tokio::test]
async fn a_change_the_service_reports_a_pass_late_is_read_again_and_then_left_alone() {
    let (harness, history) = quiet_indexer(5 * ETH).await.unwrap();
    harness.sync().await.unwrap();
    harness.drain();

    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(6 * ETH))));
    harness.sync().await.unwrap();
    assert!(harness.records().is_empty(), "the service has nothing yet, so nothing is guessed at");

    history.serve(&payment_in(TIP), &fixture("etherscan.json", "empty").unwrap()).unwrap();
    let before = history.reads().len();
    harness.sync().await.unwrap();
    assert_eq!(history.reads().len(), before.saturating_add(2), "unreported news is read again");
    let records = harness.records();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records.first().map(|record| record.detection), Some(TxDetection::Full));

    harness.sync().await.unwrap();
    assert_eq!(history.reads().len(), before.saturating_add(2), "reported news is not read again");
    assert!(harness.records().is_empty());
}

/// A solver sent the transaction that moved the owner's tokens.
///
/// That is how a gasless swap settles. The `tokentx` row's `from` is the token's
/// sender and its gas numbers are the solver's transaction, and `txlist` has no row
/// for it, because the owner sent nothing.
#[tokio::test]
async fn a_token_transfer_someone_else_sent_is_recorded_without_their_fee() {
    let pulled = serde_json::json!({
        "status": "1",
        "message": "OK",
        "result": [{
            "blockNumber": "18000001",
            "timeStamp": "1692000012",
            "hash": format!("0x{}", "5c".repeat(32)),
            "nonce": "4021",
            "from": OWNER,
            "contractAddress": common::USDT,
            "to": "0x9008D19f58AAbD9eD0D60971565AA8510560ab41",
            "value": "2500000",
            "tokenDecimal": "6",
            "gasPrice": "20000000000",
            "gasUsed": "600000",
        }],
    })
    .to_string();
    let history = FakeHistory::new();
    history.serve(&fixture("etherscan.json", "empty").unwrap(), &pulled).unwrap();
    let harness = Harness::new(Setup::with_history(history)).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&[Some(ETH); SHEET_ENTRIES])));

    harness.sync().await.unwrap();
    let records = harness.records();
    let token = records
        .iter()
        .find(|record| record.asset == AssetId::Token(TokenId::UsdtErc20))
        .expect("the token transfer is recorded");
    assert_eq!(
        token.fee, None,
        "only a transaction's sender pays its gas, and the owner sent no transaction here, yet \
         the owner's record carries the solver's fee: {token:?}"
    );
}

/// The owner's token transfer is its own call to the token contract, so `txlist`
/// lists it too, moving no coin. The store keeps the token record, which carries the
/// same fee, in place of the call's.
#[tokio::test]
async fn a_token_transfer_the_owner_sent_carries_its_fee_and_explains_the_coin_it_took() {
    let only_the_send = |key: &str| -> Fallible<String> {
        let mut page: Value = serde_json::from_str(&fixture("etherscan.json", key)?)?;
        page.get_mut("result").and_then(Value::as_array_mut).ok_or("a page of rows")?.retain(
            |row| {
                row.get("hash")
                    .and_then(Value::as_str)
                    .is_some_and(|hash| hash.starts_with("0x22e0"))
            },
        );
        Ok(page.to_string())
    };
    let history = FakeHistory::new();
    history.serve(&only_the_send("txlist").unwrap(), &only_the_send("tokentx").unwrap()).unwrap();
    let harness = Harness::new(Setup::with_history(history)).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH))));
    harness.sync().await.unwrap();

    let records = harness.records();
    assert_eq!(records.len(), 2, "the token record and the call's: {records:?}");
    let of_asset = |asset: AssetId| {
        records.iter().find(|record| record.asset == asset).expect("a record of the asset")
    };
    let fee_wei = 65_000 * 20_000_000_000;
    let sent = of_asset(AssetId::Token(TokenId::UsdtErc20));
    assert_eq!(sent.direction, TxDirection::Outgoing);
    assert_eq!(sent.amount.base_units(), 1_000_000);
    assert_eq!(sent.counterparty.as_deref(), Some(PEER));
    assert_eq!(sent.fee.map(Amount::base_units), Some(fee_wei));
    let call = of_asset(AssetId::Native(ChainId::Ethereum));
    assert_eq!(call.txid, sent.txid);
    assert_eq!(call.amount, Amount::ZERO);
    assert_eq!(call.fee.map(Amount::base_units), Some(fee_wei));

    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheet(5 * ETH - fee_wei))));
    harness.sync().await.unwrap();
    let caught_up = TIP + ETHEREUM.indexer_lag_blocks();
    harness.node.always(Method::BlockNumber, Answer::ok(&quantity(u128::from(caught_up))));
    harness.sync().await.unwrap();
    let records = harness.records();
    assert!(
        records.iter().all(|record| record.detection == TxDetection::Full),
        "the send's own row accounts for the fee, so nothing is guessed at: {records:?}"
    );
}

/// The OP Stack charges for posting a transaction's data to Ethereum apart from gas.
///
/// The service's rows leave that out, so the owner's send is priced by its receipt,
/// and a payment someone else sent is asked about nothing.
#[tokio::test]
async fn an_op_stack_send_is_priced_by_its_receipt_rather_than_its_gas() {
    let sent = format!("0x{}", "5d".repeat(32));
    let paid = format!("0x{}", "5e".repeat(32));
    let usdc = TokenId::UsdcBase.contract();
    let gas_price_wei: u128 = 1_000_000;
    let native_row = |hash: &str, from: &str, to: &str, value: u128| {
        serde_json::json!({
            "blockNumber": "18000001",
            "timeStamp": "1692000012",
            "hash": hash,
            "from": from,
            "to": to,
            "value": value.to_string(),
            "gasUsed": "65000",
            "gasPrice": gas_price_wei.to_string(),
            "isError": "0",
        })
    };
    let txlist = serde_json::json!({
        "status": "1",
        "message": "OK",
        "result": [native_row(&sent, OWNER, usdc, 0), native_row(&paid, PEER, OWNER, ETH)],
    });
    let tokentx = serde_json::json!({
        "status": "1",
        "message": "OK",
        "result": [{
            "blockNumber": "18000001",
            "timeStamp": "1692000012",
            "hash": sent,
            "from": OWNER,
            "contractAddress": usdc,
            "to": PEER,
            "value": "1000000",
            "gasUsed": "65000",
            "gasPrice": gas_price_wei.to_string(),
        }],
    });
    let history = FakeHistory::new();
    history.serve(&txlist.to_string(), &tokentx.to_string()).unwrap();
    let harness = Harness::new(Setup { history: Some(history), ..Setup::on(BASE) }).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::Call, Answer::ok(&aggregate(&sheets_on(BASE, 1, ETH))));
    let receipt = serde_json::json!({
        "transactionHash": sent,
        "status": "0x1",
        "gasUsed": "0xfde8",
        "effectiveGasPrice": format!("{gas_price_wei:#x}"),
        "l1Fee": format!("{L1_FEE_WEI:#x}"),
        "blockNumber": "0x112a7e1",
        "blockHash": format!("0x{}", "aa".repeat(32)),
    });
    harness.node.always(Method::GetTransactionReceipt, Answer::ok(&receipt.to_string()));
    harness.sync().await.unwrap();

    let records = harness.records();
    let fee_wei = 65_000 * gas_price_wei + L1_FEE_WEI;
    let of_asset = |asset: AssetId| {
        records.iter().find(|record| record.asset == asset).expect("a record of the asset")
    };
    let token = of_asset(AssetId::Token(TokenId::UsdcBase));
    assert_eq!(token.fee.map(Amount::base_units), Some(fee_wei), "{records:?}");
    let native: Vec<_> =
        records.iter().filter(|record| record.asset == AssetId::Native(ChainId::Base)).collect();
    let call = native.iter().find(|record| record.txid == token.txid).expect("the call's record");
    assert_eq!(call.fee.map(Amount::base_units), Some(fee_wei), "the call is the same send");
    let payment = native.iter().find(|record| record.txid != token.txid).expect("the payment");
    assert_eq!(payment.fee, None, "the sender paid, not us");
    assert_eq!(
        harness.node.calls_to(Method::GetTransactionReceipt).len(),
        1,
        "only the owner's own send is asked about"
    );
}

/// Etherscan's free plan stopped covering BNB Smart Chain. The key still serves
/// Ethereum, so the adapter is kept and walks logs instead, without failing a pass
/// or asking again.
#[tokio::test]
async fn a_free_key_on_bnb_smart_chain_falls_back_to_logs_and_stops_asking() {
    let history = FakeHistory::new();
    history.refuse(&fixture("etherscan.json", "free_tier_chain").unwrap());
    let harness =
        Harness::new(Setup { history: Some(Arc::clone(&history)), ..Setup::on(BSC) }).unwrap();
    harness.watch_owner().await.unwrap();
    harness.node.quiet(&sheets_on(BSC, 1, ETH));

    harness.sync().await.unwrap();
    assert_eq!(history.reads().len(), 1, "one refusal decides it");
    assert!(!harness.node.calls_to(Method::GetLogs).is_empty(), "the same pass walks logs instead");
    harness.drain();
    harness.node.forget_calls();

    harness.node.quiet(&sheets_on(BSC, 1, 2 * ETH));
    harness.sync().await.unwrap();
    assert_eq!(history.reads().len(), 1, "the service is not asked again");
    assert!(history.internal_reads().is_empty());
    assert!(!harness.node.calls_to(Method::GetLogs).is_empty());
    let records = harness.records();
    assert!(
        records.iter().any(|record| record.detection == TxDetection::BalanceDelta),
        "native activity shows the way it does without a key: {records:?}"
    );
}

/// Every list is read on to the tip, so a call in the newest blocks is not missed.
#[tokio::test]
async fn an_internal_read_names_the_chain_and_the_key_and_no_last_block() {
    let owner: Address = OWNER.parse().unwrap();
    let service = LoopbackService::start(Vec::new()).await.unwrap();
    let pool = http_pool(ChainId::Bsc, &[&service.url]).unwrap();
    let etherscan = EtherscanHistory::new(pool, "a-key", BSC).unwrap();
    timeout(Duration::from_secs(20), etherscan.internal(owner, 10))
        .await
        .expect("the read finished inside its budget")
        .expect("the service answered");
    let lines = service.finish().await.unwrap();
    let line = lines.first().expect("the service was asked");
    assert!(line.contains("chainid=56&"), "{line}");
    assert!(line.contains("action=txlistinternal&"), "{line}");
    assert!(line.contains("&startblock=10&"), "{line}");
    assert!(!line.contains("endblock"), "{line}");
    assert!(line.contains("&apikey=a-key"), "{line}");
}

#[tokio::test]
async fn a_keyless_service_is_sent_no_key() {
    let owner: Address = OWNER.parse().unwrap();
    let service = LoopbackService::start(Vec::new()).await.unwrap();
    let pool = http_pool(ChainId::Avalanche, &[&service.url]).unwrap();
    let routescan = EtherscanHistory::keyless(pool, AVALANCHE);
    assert!(format!("{routescan:?}").contains("has_key: false"), "{routescan:?}");
    timeout(Duration::from_secs(20), routescan.native(owner, 10))
        .await
        .expect("the read finished inside its budget")
        .expect("the service answered");
    let lines = service.finish().await.unwrap();
    let line = lines.first().expect("the service was asked");
    assert!(line.contains("action=txlist&"), "{line}");
    assert!(line.contains("&startblock=10&"), "{line}");
    assert!(!line.contains("apikey"), "{line}");
}
