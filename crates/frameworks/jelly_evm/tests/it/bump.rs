use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, ChainId, FeeLevel, FeeQuote, FeeSelection,
    SignedTx, TxDirection, TxId, TxOutcome, TxWarningKind, UnsignedTx, test_util::TxRequestBuilder,
};
use jelly_evm::Method;

use crate::common::{
    ARBITRUM, Answer, DEEP_ETHEREUM, ETH, Fallible, GWEI, Harness, PEER, SIGNER, Setup, TEST_KEY,
    detail, evm_fee, gwei_text, key_material, payment, quantity, receipt_for, sheet, watched,
};

const NONCE: u64 = 7;
/// The original [`payment`] pays 30 gwei with a 1 gwei tip, so a replacement has
/// to clear 33 gwei and 1.1 gwei.
const CAP_WEI: u128 = 30 * GWEI;
const TIP_WEI: u128 = GWEI;

/// A wallet whose one address has built, signed and sent a payment.
struct Sent {
    harness: Harness,
    signed: SignedTx,
}

async fn sent() -> Fallible<Sent> {
    sent_with(Setup::default()).await
}

async fn sent_with(setup: Setup) -> Fallible<Sent> {
    let harness = Harness::signer(setup, NONCE).await?;
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(21_000)));
    let signed = harness.send(&payment(PEER, NONCE)).await?;
    Ok(Sent { harness, signed })
}

impl Sent {
    async fn quote(&self, fee: &FeeSelection) -> Result<FeeQuote, ChainError> {
        let bump = self
            .harness
            .adapter
            .fee_bump()
            .ok_or(ChainError::internal_without_source("no fee bump"))?;
        bump.quote_bump(&self.signed.txid, fee).await
    }

    async fn bump(&self, fee: &FeeSelection) -> Result<UnsignedTx, ChainError> {
        let bump = self
            .harness
            .adapter
            .fee_bump()
            .ok_or(ChainError::internal_without_source("no fee bump"))?;
        bump.build_bump(&self.signed.txid, fee).await
    }

    async fn quote_cancel(&self, fee: &FeeSelection) -> Result<FeeQuote, ChainError> {
        let cancel = self
            .harness
            .adapter
            .tx_cancel()
            .ok_or(ChainError::internal_without_source("no cancel"))?;
        cancel.quote_cancel(&self.signed.txid, fee).await
    }

    async fn cancel(&self, fee: &FeeSelection) -> Result<UnsignedTx, ChainError> {
        let cancel = self
            .harness
            .adapter
            .tx_cancel()
            .ok_or(ChainError::internal_without_source("no cancel"))?;
        cancel.build_cancel(&self.signed.txid, fee).await
    }

    /// Signs `unsigned` with the test key and broadcasts it to a node that echoes
    /// its txid.
    async fn send(&self, unsigned: &UnsignedTx) -> Fallible<SignedTx> {
        let signed = self.harness.adapter.sign_tx(unsigned, &[key_material(TEST_KEY)?])?;
        self.harness
            .node
            .on(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));
        self.harness.adapter.broadcast(&signed).await?;
        Ok(signed)
    }
}

/// The pair a quote's custom fee asks for, as wei per gas.
fn pair_of(fee: Option<&FeeSelection>) -> Fallible<(u128, u128)> {
    let Some(FeeSelection::Evm { max_fee_per_gas, max_priority_fee_per_gas, .. }) = fee else {
        return Err("an evm quote offers an evm custom fee".into());
    };
    let cap = max_fee_per_gas.as_deref().ok_or("a quote pins its cap")?.parse::<u128>()?;
    let tip = max_priority_fee_per_gas.as_deref().ok_or("a quote pins its tip")?.parse::<u128>()?;
    Ok((cap, tip))
}

#[tokio::test]
async fn a_normal_quote_clears_what_the_node_charges_to_replace() {
    let sent = sent().await.unwrap();
    let quote = sent.quote(&FeeSelection::NORMAL).await.unwrap();

    // Geth and its forks need both halves at least 10 percent higher.
    let min_cap = CAP_WEI.saturating_mul(110).saturating_div(100);
    let min_tip = TIP_WEI.saturating_mul(110).saturating_div(100);
    let (cap, tip) = pair_of(quote.custom.as_ref()).unwrap();
    assert!(cap >= min_cap, "{cap} is below {min_cap}");
    assert!(tip >= min_tip, "{tip} is below {min_tip}");
}

#[tokio::test]
async fn a_quoted_custom_fee_sent_straight_back_builds_at_its_price() {
    let sent = sent().await.unwrap();
    let quote = sent.quote(&FeeSelection::NORMAL).await.unwrap();

    let custom = quote.custom.expect("an evm quote names its custom fee");
    let replacement = sent.bump(&custom).await.unwrap();
    assert_eq!(replacement.preview.fee, quote.fee);
    assert_eq!(replacement.preview.to, PEER, "the recipient is never guessed at");
    assert_eq!(replacement.preview.from, [SIGNER]);
    assert_eq!(replacement.preview.amount.base_units(), ETH, "the payment is untouched");
    assert_eq!(replacement.chain(), ChainId::Ethereum);
}

/// The recorded market's normal level tips 1.94 gwei under a 16.34 gwei cap, so
/// the floor raises the cap and the market raises the tip.
#[tokio::test]
async fn a_normal_replacement_pays_the_normal_level_raised_to_the_floor() {
    let sent = sent().await.unwrap();
    let replacement = sent.bump(&FeeSelection::NORMAL).await.unwrap();
    let details = &replacement.preview.details;
    assert_eq!(detail(details, "Nonce"), NONCE.to_string(), "the slot is reused");
    assert_eq!(
        detail(details, "Max fee per gas"),
        format!("{} ({} wei)", gwei_text(33 * GWEI), 33 * GWEI),
        "a tenth above the thirty gwei the original pays"
    );
    assert_eq!(
        detail(details, "Max priority fee per gas"),
        format!("{} ({} wei)", gwei_text(1_940_000_000), 1_940_000_000u128),
        "the market tip, above a tenth over the original's one gwei"
    );
}

#[tokio::test]
async fn a_replacement_at_a_level_takes_that_levels_tip_and_quotes_every_level_raised() {
    let sent = sent().await.unwrap();
    let quote = sent.quote(&FeeSelection::Level { level: FeeLevel::Slow }).await.unwrap();
    assert_eq!(
        pair_of(quote.custom.as_ref()).unwrap(),
        (33 * GWEI, 1_100_000_000),
        "the slow tip is under the floor, so both halves are the floor"
    );
    let levels: Vec<FeeLevel> = quote.levels.iter().map(|row| row.level).collect();
    assert_eq!(levels, FeeLevel::ALL);
    let slow = quote.levels.first().map(|row| row.fee);
    assert_eq!(slow, Some(quote.fee), "the slow row is what the slow replacement pays");

    let fast = sent.bump(&FeeSelection::Level { level: FeeLevel::Fast }).await.unwrap();
    assert_eq!(
        detail(&fast.preview.details, "Max priority fee per gas"),
        format!("{} ({} wei)", gwei_text(2_940_000_000), 2_940_000_000u128)
    );
}

#[tokio::test]
async fn a_quote_resolves_the_replacement_once() {
    let sent = sent().await.unwrap();
    sent.harness.node.forget_calls();
    sent.quote(&FeeSelection::NORMAL).await.unwrap();
    assert_eq!(sent.harness.node.calls_to(Method::GetTransactionCount).len(), 1);
    assert_eq!(sent.harness.node.calls_to(Method::FeeHistory).len(), 1);
    assert_eq!(sent.harness.node.calls_to(Method::EstimateGas).len(), 1);
}

#[tokio::test]
async fn a_replacement_that_does_not_outbid_the_original_is_refused() {
    let sent = sent().await.unwrap();
    let underpriced = evm_fee(Some(21_000), Some(20 * GWEI), Some(TIP_WEI));
    let error = sent.quote(&underpriced).await.expect_err("a replacement has to pay more");
    assert!(
        matches!(error, ChainError::InvalidInput(ref text, _) if text.contains("set at least")),
        "the quote refuses it before review: {error:?}"
    );
    let error = sent
        .bump(&evm_fee(Some(21_000), Some(CAP_WEI), Some(TIP_WEI)))
        .await
        .expect_err("a replacement has to pay more");
    assert!(error.to_string().contains("set at least"), "{error}");
}

#[tokio::test]
async fn a_replacement_at_a_custom_fee_keeps_the_originals_slot_and_call() {
    let sent = sent().await.unwrap();
    let custom = evm_fee(Some(21_000), Some(100 * GWEI), Some(10 * GWEI));
    let replacement = sent.bump(&custom).await.unwrap();
    let details = &replacement.preview.details;
    assert_eq!(
        detail(details, "Nonce"),
        NONCE.to_string(),
        "the slot came from the transaction, not from the caller"
    );
    assert_eq!(detail(details, "Calldata"), "None", "and neither did the call");
    assert_eq!(replacement.preview.to, PEER);
    assert_eq!(replacement.preview.amount.base_units(), ETH);
}

#[tokio::test]
async fn a_transaction_this_adapter_is_not_following_cannot_be_replaced() {
    let sent = sent().await.unwrap();
    let stranger = TxId::new(ChainId::Ethereum, format!("0x{}", "ab".repeat(32)));
    sent.bump(&FeeSelection::NORMAL).await.expect("the wallet's own send is replaceable");

    let bump = sent.harness.adapter.fee_bump().expect("ethereum can replace a transaction");
    let refused = bump
        .quote_bump(&stranger, &FeeSelection::NORMAL)
        .await
        .expect_err("it is not ours to replace");
    assert!(refused.to_string().contains("not pending in this wallet"), "{refused}");
    let refused = bump
        .build_bump(&stranger, &FeeSelection::NORMAL)
        .await
        .expect_err("it is not ours to replace");
    assert!(refused.to_string().contains("not pending in this wallet"), "{refused}");
}

#[tokio::test]
async fn a_transaction_a_pass_has_seen_in_a_block_cannot_be_replaced() {
    let sent = sent_with(Setup::on(&DEEP_ETHEREUM).one_window()).await.unwrap();
    let harness = &sent.harness;

    harness.node.quiet(&sheet(4 * ETH));
    harness.node.always(
        Method::GetTransactionReceipt,
        Answer::ok(&receipt_for(sent.signed.txid.value(), 18_000_001)),
    );
    harness.sync().await.unwrap();
    assert_eq!(harness.adapter.in_flight(), 1, "it is still watched");

    let refused = sent
        .quote(&FeeSelection::NORMAL)
        .await
        .expect_err("a confirmed transaction has no slot to reuse");
    assert!(refused.to_string().contains("already confirmed"), "{refused}");
    let refused = sent
        .bump(&FeeSelection::NORMAL)
        .await
        .expect_err("a confirmed transaction has no slot to reuse");
    assert!(refused.to_string().contains("already confirmed"), "{refused}");
}

#[tokio::test]
async fn a_replaced_send_is_settled_as_replaced_once_its_replacement_is_final() {
    let sent = sent_with(Setup::default().one_window()).await.unwrap();
    let harness = &sent.harness;
    let replacement = sent.bump(&FeeSelection::NORMAL).await.unwrap();
    let signed = harness.adapter.sign_tx(&replacement, &[key_material(TEST_KEY).unwrap()]).unwrap();
    harness
        .node
        .always(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));
    harness.adapter.broadcast(&signed).await.unwrap();
    harness.drain();

    harness.node.quiet(&sheet(4 * ETH));
    // The original has no receipt, and the replacement is mined far below the tip.
    harness.node.on(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.on(
        Method::GetTransactionReceipt,
        Answer::ok(&receipt_for(signed.txid.value(), 18_000_001)),
    );
    harness.sync().await.unwrap();

    let records = harness.records();
    let original = records.iter().find(|record| record.txid == sent.signed.txid);
    assert!(
        original.is_some_and(|record| record.outcome == TxOutcome::Replaced
            && record.amount == Amount::ZERO
            && record.fee == Some(Amount::ZERO)),
        "once its replacement is final the original can never confirm: {records:?}"
    );
    assert_eq!(harness.adapter.in_flight(), 0, "neither is watched any more");
}

#[tokio::test]
async fn a_cancel_sends_nothing_to_its_own_sender_in_the_originals_slot() {
    let sent = sent().await.unwrap();
    sent.harness.node.forget_calls();
    let cancel = sent.cancel(&FeeSelection::NORMAL).await.unwrap();
    let preview = &cancel.preview;
    assert_eq!(preview.from, [SIGNER]);
    assert_eq!(preview.to, SIGNER, "a cancel pays the sender itself");
    assert_eq!(preview.amount, Amount::ZERO);
    assert_eq!(preview.asset, AssetId::Native(ChainId::Ethereum));
    assert_eq!(detail(&preview.details, "Nonce"), NONCE.to_string(), "the slot is reused");
    assert_eq!(detail(&preview.details, "Calldata"), "None");
    assert_eq!(detail(&preview.details, "Value in wei"), "0");
    assert_eq!(detail(&preview.details, "Gas limit source"), "Estimated by the node");
    assert_eq!(
        detail(&preview.details, "Max fee per gas"),
        format!("{} ({} wei)", gwei_text(33 * GWEI), 33 * GWEI),
        "a cancel clears the floor a speed-up must"
    );
    let replacement = preview
        .warnings
        .iter()
        .find(|warning| warning.kind == TxWarningKind::Replacement)
        .expect("a cancel is a replacement");
    assert!(replacement.message.starts_with("This cancels the transaction at nonce 7"));
    assert!(
        preview.warnings.iter().all(|warning| !warning.message.contains("amount is zero")
            && !warning.message.contains("same address")),
        "a cancel is meant to move nothing to itself: {:?}",
        preview.warnings
    );

    let estimates = sent.harness.node.calls_to(Method::EstimateGas);
    let [estimate] = estimates.as_slice() else { panic!("one estimate: {estimates:?}") };
    let params = estimate.params.to_ascii_lowercase();
    assert!(params.contains(&format!(r#""to":"{}""#, SIGNER.to_ascii_lowercase())), "{params}");
    assert!(!params.contains("value") && !params.contains("input"), "{params}");
}

#[tokio::test]
async fn a_cancel_of_a_call_drops_its_calldata() {
    let harness = Harness::signer(Setup::default(), NONCE).await.unwrap();
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(50_000)));
    let mut call = payment(PEER, NONCE);
    call.fee = evm_fee(Some(50_000), Some(CAP_WEI), Some(TIP_WEI));
    call.nonce = None;
    call.data = Some(String::from("0xd0e30db0"));
    let signed = harness.send(&call).await.unwrap();
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(21_000)));

    let cancel = harness.adapter.tx_cancel().expect("ethereum can cancel");
    let unsigned = cancel.build_cancel(&signed.txid, &FeeSelection::NORMAL).await.unwrap();
    assert_eq!(detail(&unsigned.preview.details, "Calldata"), "None");
    assert_eq!(detail(&unsigned.preview.details, "Transaction to"), SIGNER);
    assert_eq!(detail(&unsigned.preview.details, "Gas limit"), "21000");
}

/// Arbitrum charges an empty transfer its L1 cost in gas, so a cancel there takes
/// whatever the node estimates rather than the intrinsic 21,000.
#[tokio::test]
async fn an_arbitrum_cancel_pays_the_gas_its_estimate_names() {
    let harness = Harness::new(Setup::on(ARBITRUM)).unwrap();
    harness.watch(&[watched(7, ChainId::Arbitrum, SIGNER)]).await.unwrap();
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(u128::from(NONCE))));
    harness.node.always(Method::GetBalance, Answer::ok(&quantity(5 * ETH)));
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(21_000)));
    let original = TxRequestBuilder::new(
        7,
        PEER,
        AssetId::Native(ChainId::Arbitrum),
        Amount::from_base_units(ETH),
    )
    .fee(evm_fee(Some(21_000), Some(CAP_WEI), Some(TIP_WEI)))
    .nonce(NONCE)
    .build();
    let signed = harness.send(&original).await.unwrap();
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(120_000)));

    let cancel = harness.adapter.tx_cancel().expect("arbitrum can cancel");
    let quote = cancel.quote_cancel(&signed.txid, &FeeSelection::NORMAL).await.unwrap();
    let Some(FeeSelection::Evm { gas_limit, .. }) = quote.custom else {
        panic!("an evm quote names an evm custom fee")
    };
    assert_eq!(gas_limit.as_deref(), Some("144000"), "the estimate and a fifth again");
}

#[tokio::test]
async fn a_cancel_that_does_not_outbid_the_original_is_refused() {
    let sent = sent().await.unwrap();
    let error = sent
        .quote_cancel(&evm_fee(None, Some(CAP_WEI), Some(TIP_WEI)))
        .await
        .expect_err("a cancel has to pay more");
    assert!(
        matches!(error, ChainError::InvalidInput(ref text, _) if text.contains("set at least"))
    );
}

#[tokio::test]
async fn a_cancel_of_a_send_this_adapter_is_not_following_is_refused() {
    let sent = sent().await.unwrap();
    let stranger = TxId::new(ChainId::Ethereum, format!("0x{}", "ab".repeat(32)));
    let cancel = sent.harness.adapter.tx_cancel().expect("ethereum can cancel");
    let refused =
        cancel.build_cancel(&stranger, &FeeSelection::NORMAL).await.expect_err("not ours");
    assert!(refused.to_string().contains("not pending in this wallet"), "{refused}");
}

/// Once the cancel lands at depth, the original never can, so it's recorded as
/// replaced and the cancel as a self-transfer that paid the fee.
#[tokio::test]
async fn a_cancel_that_lands_replaces_the_original_and_is_recorded_as_a_paid_self_transfer() {
    let sent = sent_with(Setup::default().one_window()).await.unwrap();
    let harness = &sent.harness;
    let cancel = sent.cancel(&FeeSelection::NORMAL).await.unwrap();
    let signed = sent.send(&cancel).await.unwrap();
    harness.drain();

    harness.node.quiet(&sheet(5 * ETH - 21_000 * 20 * GWEI));
    harness.node.always(Method::GetTransactionCount, Answer::ok(&quantity(u128::from(NONCE) + 1)));
    harness.node.on(Method::GetTransactionReceipt, Answer::ok("null"));
    harness.node.on(
        Method::GetTransactionReceipt,
        Answer::ok(&receipt_for(signed.txid.value(), 18_000_001)),
    );
    harness.sync().await.unwrap();

    let records = harness.records();
    let original = records.iter().find(|record| record.txid == sent.signed.txid);
    assert!(
        original.is_some_and(|record| record.outcome == TxOutcome::Replaced
            && record.amount == Amount::ZERO
            && record.fee == Some(Amount::ZERO)),
        "{records:?}"
    );
    let landed = records.iter().find(|record| record.txid == signed.txid);
    assert!(
        landed.is_some_and(|record| record.direction == TxDirection::SelfTransfer
            && record.outcome == TxOutcome::Success
            && record.amount == Amount::ZERO
            && record.fee == Some(Amount::from_base_units(21_000 * 20 * GWEI))),
        "{records:?}"
    );
    assert_eq!(harness.adapter.in_flight(), 0);
}

/// A contract call the wallet sent with calldata, such as a WETH deposit, then sped
/// up.
#[tokio::test]
async fn a_fee_bump_of_a_calldata_send_keeps_the_calldata() {
    const DEPOSIT: &str = "0xd0e30db0";
    let harness = Harness::signer(Setup::default(), NONCE).await.unwrap();
    harness.node.always(Method::EstimateGas, Answer::ok(&quantity(50_000)));
    let mut call = payment(PEER, NONCE);
    call.fee = evm_fee(Some(50_000), Some(CAP_WEI), Some(TIP_WEI));
    call.nonce = None;
    call.data = Some(String::from(DEPOSIT));
    let original = harness.adapter.build_tx(&call).await.unwrap();
    assert_eq!(detail(&original.preview.details, "Calldata"), DEPOSIT, "the call that was sent");
    let signed = harness.adapter.sign_tx(&original, &[key_material(TEST_KEY).unwrap()]).unwrap();
    harness
        .node
        .on(Method::SendRawTransaction, Answer::ok(&format!("\"{}\"", signed.txid.value())));
    harness.adapter.broadcast(&signed).await.unwrap();

    let bump = harness.adapter.fee_bump().expect("ethereum can replace a transaction");
    let replacement = bump.build_bump(&signed.txid, &FeeSelection::NORMAL).await.unwrap();
    let details = &replacement.preview.details;
    assert_eq!(detail(details, "Nonce"), NONCE.to_string(), "the slot is reused");
    assert_eq!(
        detail(details, "Calldata"),
        DEPOSIT,
        "a speed-up must be the same call at a higher price, but the replacement the user is \
         asked to sign carries calldata {:?} to {}, which cancels the original call",
        detail(details, "Calldata"),
        detail(details, "Transaction to"),
    );
}

#[tokio::test]
async fn a_transaction_id_that_is_not_one_is_refused_before_anything_is_fetched() {
    let sent = sent().await.unwrap();
    let bump = sent.harness.adapter.fee_bump().expect("ethereum can replace a transaction");
    let broken = TxId::new(ChainId::Ethereum, "not a transaction id");
    sent.harness.node.forget_calls();
    bump.quote_bump(&broken, &FeeSelection::NORMAL).await.unwrap_err();
    bump.build_bump(&broken, &FeeSelection::NORMAL).await.unwrap_err();
    assert!(sent.harness.node.calls().is_empty(), "nothing was fetched");
}
