//! Rebuilds of two recorded mainnet transactions, whose bytes come from outside this
//! workspace.
//!
//! Each runs through the shipped adapter over a head block rebuilt from the
//! transaction's own TAPOS reference, so a match proves we produce the bytes the
//! network accepted.

use jelly_core::ChainAdapter as _;
use jelly_tron::{Route, TronConfig};
use serde_json::Value;

use crate::common::{
    self, Answer, FakeNode, Harness, TRX, USDT_ASSET, account, base, canonical, detail, fixture,
    raw_data_hex, recorded, request, text, transfer_arguments, watched,
};

#[tokio::test]
async fn a_rebuild_reproduces_the_recorded_mainnet_trx_transfer() {
    let tx = recorded(fixture!("tx_trx")).unwrap();
    let owner = canonical(&tx.value().owner_address).unwrap();
    let recipient = canonical(tx.value().to_address.as_deref().unwrap_or_default()).unwrap();
    let amount = u128::from(tx.value().amount.unwrap_or_default());

    let fake = FakeNode::new();
    let fake = base(&fake)
        .always(Route::NowBlock, Answer::body(fixture!("now_block_trx")))
        .on_matching(Route::Account, &owner, account(&owner, 100_000_000))
        .on_matching(Route::Account, &recipient, account(&recipient, 1));
    let harness = Harness::with(
        &fake,
        TronConfig { expiration: tx.window().unwrap(), ..TronConfig::default() },
    );
    harness.adapter.set_watch_set(&[watched(1, &owner)]).await.unwrap();

    let unsigned = harness.adapter.build_tx(&request(TRX, &recipient, amount)).await.unwrap();
    assert_eq!(raw_data_hex(&unsigned).unwrap(), tx.raw_data_hex);
    assert_eq!(detail(&unsigned.preview.details, "Transaction id").unwrap(), tx.txid);
}

#[tokio::test]
async fn a_rebuild_reproduces_the_recorded_mainnet_usdt_transfer() {
    let tx = recorded(fixture!("tx_usdt")).unwrap();
    let owner = canonical(&tx.value().owner_address).unwrap();
    let calldata = hex::decode(tx.value().data.clone().unwrap_or_default()).unwrap();
    let (recipient_body, amount) = transfer_arguments(&calldata).unwrap();
    let recipient = canonical(&hex::encode(recipient_body)).unwrap();
    let fee_limit = u64::try_from(tx.raw_data.fee_limit).unwrap();

    let fake = FakeNode::new();
    let fake = base(&fake)
        .always(Route::NowBlock, Answer::body(fixture!("now_block_usdt")))
        .on_matching(Route::Account, &owner, account(&owner, 100_000_000))
        .on_matching(
            Route::ConstantContract,
            "balanceOf(address)",
            Answer::body(fixture!("constant_balance_of")),
        )
        .on_matching(
            Route::ConstantContract,
            "transfer(address,uint256)",
            Answer::body(fixture!("constant_transfer")),
        );
    let harness = Harness::with(
        &fake,
        TronConfig { expiration: tx.window().unwrap(), ..TronConfig::default() },
    );
    harness.adapter.set_watch_set(&[watched(1, &owner)]).await.unwrap();

    // The recorded transaction carried a 100 TRX fee limit, which is also this
    // build's floor, so an automatic rebuild reproduces it.
    assert_eq!(fee_limit, 100_000_000);
    let unsigned =
        harness.adapter.build_tx(&request(USDT_ASSET, &recipient, amount)).await.unwrap();
    assert_eq!(raw_data_hex(&unsigned).unwrap(), tx.raw_data_hex);
    assert_eq!(detail(&unsigned.preview.details, "Transaction id").unwrap(), tx.txid);

    // The receipt's transfer log is the chain's own reading of the calldata, so a
    // drifted argument order would disagree with it.
    let receipt: Value = serde_json::from_str(common::fixture!("tx_usdt_info")).unwrap();
    assert_eq!(text(&receipt, "id").unwrap(), tx.txid);
    let log = receipt.get("log").and_then(Value::as_array).and_then(|rows| rows.first());
    let log = log.expect("the transfer log");
    let topics = log.get("topics").and_then(Value::as_array).expect("the log topics");
    assert_eq!(
        topics.first().and_then(Value::as_str),
        Some("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"),
        "the ERC-20 Transfer event"
    );
    let to = topics.get(2).and_then(Value::as_str).expect("the recipient topic");
    let body_hex = hex::encode(recipient_body);
    // A log topic carries the twenty-byte EVM form, and a TRON body is the same
    // bytes behind the 0x41 prefix.
    let evm = body_hex.strip_prefix("41").expect("the TRON prefix byte");
    assert!(to.ends_with(evm), "{to}");
    assert_eq!(u128::from_str_radix(&text(log, "data").unwrap(), 16).unwrap(), amount);
}
