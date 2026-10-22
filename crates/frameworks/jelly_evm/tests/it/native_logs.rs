//! Polygon's node logs every native transfer, so the log walk records the coin's
//! history without an indexer and a balance change is never also a delta.

use jelly_core::{AssetId, ChainId, TxDetection, TxDirection};
use jelly_evm::Method;

use crate::common::{
    Answer, ETH, Fallible, Harness, OWNER, PEER, POLYGON, Setup, TIP, TIP_HEX, aggregate, quantity,
    sheets_on, watched,
};

const BOR_NATIVE_TOKEN: &str = "0x0000000000000000000000000000000000001010";

const LOG_TRANSFER: &str = "0xe6497e3ee548a3372136af2fcb0696db31fc6cf20260707645068bd3fe97f3c4";

const TXID: &str = "0x5fd95c954f348b49476646930ef10ef2c8ea122fab68ccac55c3b191e9b46409";

const POL: AssetId = AssetId::Native(ChainId::Polygon);

fn word(address: &str) -> String {
    format!("0x{:0>64}", address.trim_start_matches("0x").to_lowercase())
}

/// Bor's log of `value` wei moving from `from` to `to` in `block`, with the four
/// balance words zeroed.
fn native_log(from: &str, to: &str, value: u128, block: u64) -> String {
    format!(
        r#"[{{"address":"{BOR_NATIVE_TOKEN}","topics":["{LOG_TRANSFER}","{}","{}","{}"],
            "data":"0x{value:064x}{zeros}","blockNumber":"{block:#x}",
            "transactionHash":"{TXID}","logIndex":"0x29","removed":false}}]"#,
        word(BOR_NATIVE_TOKEN),
        word(from),
        word(to),
        zeros = "0".repeat(4 * 64),
    )
}

/// A Polygon wallet watching the owner, one quiet pass in at five coins.
async fn quiet_wallet() -> Fallible<Harness> {
    let harness = Harness::new(Setup::on(POLYGON).one_window())?;
    harness.watch(&[watched(7, ChainId::Polygon, OWNER)]).await?;
    harness.node.always(Method::BlockNumber, Answer::ok(TIP_HEX));
    harness.node.always(Method::GetLogs, Answer::ok("[]"));
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheets_on(POLYGON, 1, 5 * ETH))));
    harness.sync().await?;
    harness.drain();
    harness.node.forget_calls();
    Ok(harness)
}

/// The filters go out as tokens sent, tokens received, coin sent and coin
/// received, so the fourth answer is the payment.
fn answer_payment(harness: &Harness, block: u64) {
    for _ in 0..3 {
        harness.node.on(Method::GetLogs, Answer::ok("[]"));
    }
    harness.node.on(Method::GetLogs, Answer::ok(&native_log(PEER, OWNER, 2 * ETH, block)));
}

#[tokio::test]
async fn a_native_payment_the_walk_finds_is_a_full_record_and_never_a_delta() {
    let harness = quiet_wallet().await.unwrap();
    harness.node.always(Method::BlockNumber, Answer::ok(&quantity(u128::from(TIP + 10))));
    answer_payment(&harness, TIP + 5);
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheets_on(POLYGON, 1, 7 * ETH))));
    harness.sync().await.unwrap();

    let records = harness.records();
    assert_eq!(records.len(), 1, "one transfer, and no delta beside it: {records:?}");
    let record = records.first().expect("the payment");
    assert_eq!(record.asset, POL);
    assert_eq!(record.detection, TxDetection::Full);
    assert_eq!(record.direction, TxDirection::Incoming);
    assert_eq!(record.amount.base_units(), 2 * ETH);
    assert_eq!(record.counterparty.as_deref(), Some(PEER));
    assert_eq!(record.txid.value(), TXID);
    assert_eq!(record.height, Some(TIP + 5));

    let filters = harness.node.calls_to(Method::GetLogs);
    assert_eq!(filters.len(), 4, "both sides of the tokens and of the coin");
    for filter in filters.iter().skip(2) {
        assert!(filter.params.contains(BOR_NATIVE_TOKEN), "{}", filter.params);
        assert!(filter.params.contains(LOG_TRANSFER), "{}", filter.params);
    }
}

/// The balance is read after the tip, so it can already hold a payment in a block
/// the walk has not reached. That payment is the walk's to record on the next
/// pass, and a delta now would record it twice.
#[tokio::test]
async fn a_change_the_walk_has_not_reached_yet_waits_for_the_walk_instead_of_becoming_a_delta() {
    let harness = quiet_wallet().await.unwrap();
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheets_on(POLYGON, 1, 7 * ETH))));
    harness.sync().await.unwrap();
    assert!(harness.records().is_empty(), "the tip has not moved, so nothing explains it yet");

    harness.node.always(Method::BlockNumber, Answer::ok(&quantity(u128::from(TIP + 3))));
    answer_payment(&harness, TIP + 1);
    harness.node.on(Method::Call, Answer::ok(&aggregate(&sheets_on(POLYGON, 1, 7 * ETH))));
    harness.sync().await.unwrap();

    let records = harness.records();
    assert_eq!(records.len(), 1, "{records:?}");
    let record = records.first().expect("the payment");
    assert_eq!(record.detection, TxDetection::Full);
    assert_eq!(record.amount.base_units(), 2 * ETH);
}
