//! Replacing an unconfirmed payment with a higher-paying one.
//!
//! The BIP-125 arithmetic belongs to the engine. These tests prove which address
//! funds the replacement and who it keeps paying, both read off the transaction
//! and the watch set, and that the floor follows what the server charges rather
//! than the compiled constant.

use std::slice::from_ref;
use std::sync::Arc;
use std::time::Duration;

use bitcoin::Transaction;
use bitcoin::transaction::Version;
use jelly_core::{
    ChainAdapter as _, ChainError, FeeQuote, FeeRate, FeeSelection, ScriptKind, SignedTx, TxId,
    TxOutcome, TxWarningKind, UnsignedTx, test_util::records,
};
use jelly_electrum::Method;
use jelly_utxo::{
    UtxoParams,
    test_util::{claim_owner, claimed, tx_paying, tx_spending},
};

use crate::common::{
    BITCOIN, CHANGE_VOUT, Fallible, Harness, Ledger, MINED_AT, MINED_LATER, Setup, Wallet, at,
    decode, drain, kind_of, payload, quoted, quoted_rate, sats, synced,
};

const FUNDING_SAT: u64 = 500_000;
const PAYMENT_SAT: u64 = 100_000;
/// Four base units per virtual byte, well above either chain's relay floor, which
/// leaves room to bid.
const RATE: &str = "4";
const HIGHER: FeeRate = FeeRate::from_sat_per_kwu(2_000);
const LOWER: FeeRate = FeeRate::from_sat_per_kwu(1_000);

/// Of the sat-scale tables only Bitcoin offers a replacement; tests/it/dogecoin.rs
/// covers Dogecoin's at its own scale.
const REPLACEABLE: [UtxoParams; 1] = [BITCOIN];

/// A wallet that has built, signed and sent one payment.
struct Sent {
    harness: Harness,
    wallet: Wallet,
    unsigned: UnsignedTx,
    signed: SignedTx,
    original: Transaction,
}

/// The estimator answers one base unit per virtual byte, below any replacement
/// floor, so a replacement at the normal level pays exactly the floor.
async fn sent(params: &UtxoParams) -> Fallible<Sent> {
    let (harness, wallet) = synced(Setup::on(params), kind_of(params), &[FUNDING_SAT]).await?;
    harness.serve(Method::EstimateFee, "0.00001");
    let unsigned = harness
        .adapter
        .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, RATE))
        .await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()])?;
    let original = decode(signed.raw.as_bytes())?;
    Ok(Sent { harness, wallet, unsigned, signed, original })
}

impl Sent {
    /// Serves the two fetches a replacement makes, the original and then its
    /// funding.
    fn serve_original(&self) {
        self.harness.queue_transactions(from_ref(&self.original));
        self.harness.queue_transactions(from_ref(&self.wallet.payment));
    }

    async fn bump(&self, rate: FeeRate) -> Result<UnsignedTx, ChainError> {
        self.serve_original();
        let bump = self
            .harness
            .adapter
            .fee_bump()
            .ok_or_else(|| ChainError::unsupported(String::new()))?;
        bump.build_bump(&self.signed.txid, &at(rate)).await
    }

    /// The quote makes the same two fetches a build does.
    async fn quote(&self, fee: &FeeSelection) -> Result<FeeQuote, ChainError> {
        self.serve_original();
        let bump = self
            .harness
            .adapter
            .fee_bump()
            .ok_or_else(|| ChainError::unsupported(String::new()))?;
        bump.quote_bump(&self.signed.txid, fee).await
    }
}

/// What the output a build puts change at pays.
fn change_of(tx: &Transaction) -> Fallible<u64> {
    let change =
        tx.output.get(usize::try_from(CHANGE_VOUT)?).ok_or("the transaction has no change")?;
    Ok(change.value.to_sat())
}

#[tokio::test]
async fn a_replacement_keeps_paying_the_same_recipient_and_pays_a_higher_fee() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let replacement = sent.bump(HIGHER).await.unwrap();

        let after = payload(&replacement).unwrap();
        let replaced = after.replaces.expect("the replacement records its original");
        assert_eq!(replaced.to_string(), sent.signed.txid.value());
        assert_eq!(after.to, sent.wallet.theirs.address(), "the recipient is never guessed at");
        assert_eq!(after.sources, [sent.wallet.ours.address()]);
        assert_eq!(replacement.preview.amount, sats(PAYMENT_SAT), "the payment is untouched");

        let before = payload(&sent.unsigned).unwrap();
        assert!(after.fee > before.fee, "{} is not above {}", after.fee, before.fee);
        assert!(
            replacement
                .preview
                .warnings
                .iter()
                .any(|warning| warning.kind == TxWarningKind::Replacement),
            "{:?}",
            replacement.preview.warnings
        );
    }
}

#[tokio::test]
async fn a_replacement_reuses_the_original_inputs_and_stays_replaceable() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let replacement = sent.bump(HIGHER).await.unwrap();
        let after = payload(&replacement).unwrap();

        let before: Vec<_> = sent.original.input.iter().map(|txin| txin.previous_output).collect();
        let reused: Vec<_> = after.tx.input.iter().map(|txin| txin.previous_output).collect();
        assert_eq!(reused, before, "BIP-125 rule 2 makes the original's own inputs free to keep");
        for txin in &after.tx.input {
            assert!(txin.sequence.is_rbf(), "a replacement is itself replaceable");
        }

        // The higher fee came out of the change output, not out of the payment.
        let recipient = params.script_pubkey(sent.wallet.theirs.address()).unwrap();
        let paid = after
            .tx
            .output
            .iter()
            .find(|txout| txout.script_pubkey == recipient)
            .map(|txout| txout.value.to_sat());
        assert_eq!(paid, Some(PAYMENT_SAT));
        let change_before = change_of(&sent.original).unwrap();
        let change_after = change_of(&after.tx).unwrap();
        assert!(change_after < change_before, "{change_after} is not below {change_before}");
    }
}

#[tokio::test]
async fn a_replacement_takes_over_the_claim_its_original_holds() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let held = claimed(&sent.harness.adapter);
        assert_eq!(held.len(), 1);

        let replacement = sent.bump(HIGHER).await.unwrap();
        let after = payload(&replacement).unwrap();
        assert_eq!(claimed(&sent.harness.adapter), held, "the same coins, one claim");
        assert_eq!(
            claim_owner(&sent.harness.adapter, *held.first().expect("one coin")),
            Some(after.tx.compute_txid()),
            "the claim belongs to the replacement now"
        );
    }
}

#[tokio::test]
async fn the_lowest_rate_the_network_will_accept_is_above_the_originals_own() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let minimum = quoted_rate(&sent.quote(&FeeSelection::NORMAL).await.unwrap()).unwrap();
        let asked = FeeRate::parse_sat_per_vb(RATE).unwrap();
        assert!(minimum > asked, "{minimum:?} is not above {asked:?}");
    }
}

/// BIP-125 rule 4 is priced against whatever the server charges, not against an
/// assumed compiled figure.
#[tokio::test]
async fn the_replacement_floor_follows_the_servers_incremental_relay_fee() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let compiled = quoted_rate(&sent.quote(&FeeSelection::NORMAL).await.unwrap()).unwrap();

        // A second adapter over a server charging ten times the incremental relay
        // fee.
        let raised = Harness::new(Setup::on(&params)).unwrap();
        raised.serve(Method::EstimateFee, "0.00001");
        raised.serve(
            Method::MempoolInfo,
            r#"{"mempoolminfee":0.00001,"minrelaytxfee":0.00001,"incrementalrelayfee":0.0001}"#,
        );
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .mempool(&sent.original)
            .serve(&raised);
        raised.watch(&[wallet.address()]).await.unwrap();
        raised.sync().await.unwrap();
        raised.queue_transactions(from_ref(&sent.original));
        raised.queue_transactions(from_ref(&wallet.payment));

        let bump = raised.adapter.fee_bump().expect("the adapter can replace a transaction");
        let higher =
            quoted_rate(&bump.quote_bump(&sent.signed.txid, &FeeSelection::NORMAL).await.unwrap())
                .unwrap();
        assert!(higher > compiled, "{}: {higher:?} must exceed {compiled:?}", params.chain);
    }
}

/// Real wallets replace at the current market rate, or at the network's floor when
/// the original already pays more than that.
#[tokio::test]
async fn a_replacement_at_a_level_pays_the_higher_of_the_estimate_and_the_floor() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let floor = quoted_rate(&sent.quote(&FeeSelection::NORMAL).await.unwrap()).unwrap();

        sent.harness.clock.advance(Duration::from_secs(60));
        sent.harness.serve(Method::EstimateFee, "0.0002");
        let quote = sent.quote(&FeeSelection::NORMAL).await.unwrap();
        let busy = quoted_rate(&quote).unwrap();
        assert_eq!(busy, FeeRate::from_sat_per_kwu(5_000), "the estimate is above the floor");
        assert!(busy > floor);
        let built = sent.bump(busy).await.unwrap();
        assert_eq!(built.preview.fee, quote.fee, "a quote is priced by the replacement it builds");
    }
}

#[tokio::test]
async fn a_manual_replacement_is_quoted_as_given_and_refused_below_the_floor() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let quote = sent.quote(&at(HIGHER)).await.unwrap();
        assert_eq!(quoted_rate(&quote).unwrap(), HIGHER);
        let error = sent.quote(&at(LOWER)).await.expect_err("below the floor");
        assert!(error.to_string().contains("Choose a higher fee rate"), "{error}");
    }
}

/// A send-max leaves no change and no spare coin, so a higher fee can only come
/// out of the payment, and a payment close to the dust threshold leaves it little
/// room.
#[tokio::test]
async fn a_quote_nothing_can_fund_says_so_rather_than_blaming_the_rate() {
    let (harness, wallet) = synced(Setup::on(&BITCOIN), ScriptKind::P2wpkh, &[420]).await.unwrap();
    let everything =
        drain(&BITCOIN, wallet.theirs.address(), FeeSelection::Rate { rate: "1".to_owned() });
    let unsigned = harness.adapter.build_tx(&everything).await.unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();
    let original = decode(signed.raw.as_bytes()).unwrap();

    harness.queue_transactions(from_ref(&original));
    harness.queue_transactions(from_ref(&wallet.payment));
    let bump = harness.adapter.fee_bump().expect("the adapter can replace a transaction");
    let error =
        bump.quote_bump(&signed.txid, &FeeSelection::NORMAL).await.expect_err("nothing funds it");
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error}");
}

#[tokio::test]
async fn a_replacement_that_does_not_raise_the_rate_is_refused_in_the_chains_own_unit() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let error = sent.bump(LOWER).await.expect_err("a replacement has to pay more");
        let text = error.to_string();
        assert!(text.contains("Choose a higher fee rate"), "{text}");
        assert!(
            text.contains(&format!("already pays 4 {}.", sent.harness.params.fee_rate_unit)),
            "{text}"
        );
    }
}

#[tokio::test]
async fn a_transaction_that_never_signalled_replaceability_is_refused_for_that() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let mut settled = sent.original.clone();
        for txin in &mut settled.input {
            txin.sequence = bitcoin::Sequence::MAX;
        }
        let txid = TxId::new(params.chain, settled.compute_txid().to_string());
        let bump = sent.harness.adapter.fee_bump().expect("the adapter can replace a transaction");

        sent.harness.queue_transactions(from_ref(&settled));
        sent.harness.queue_transactions(from_ref(&sent.wallet.payment));
        let quoted = bump
            .quote_bump(&txid, &FeeSelection::NORMAL)
            .await
            .expect_err("no replacement can be offered");
        assert!(quoted.to_string().contains("did not enable fee replacement"), "{quoted}");

        sent.harness.queue_transactions(from_ref(&settled));
        sent.harness.queue_transactions(from_ref(&sent.wallet.payment));
        let built = bump
            .build_bump(&txid, &at(HIGHER))
            .await
            .expect_err("nor can one be built at any price");
        assert_eq!(built.to_string(), quoted.to_string());
    }
}

#[tokio::test]
async fn a_server_answering_with_another_transaction_is_refused() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let mut substitute = sent.original.clone();
        substitute.version = Version::ONE;
        assert_ne!(substitute.compute_txid(), sent.original.compute_txid());

        sent.harness.queue_transactions(from_ref(&substitute));
        sent.harness.queue_transactions(from_ref(&sent.wallet.payment));
        let bump = sent.harness.adapter.fee_bump().expect("the adapter can replace a transaction");
        let error = bump
            .build_bump(&sent.signed.txid, &at(HIGHER))
            .await
            .expect_err("the answer is not the transaction that was asked for");
        assert!(matches!(error, ChainError::EndpointsDown { .. }), "{error}");
    }
}

#[tokio::test]
async fn a_transaction_that_does_not_spend_from_this_wallet_cannot_be_replaced() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        // A transaction of the stranger's, funded by the stranger, which the wallet
        // has no address in.
        let theirs = sent.harness.key(ScriptKind::P2wpkh, 9).unwrap();
        let elsewhere = tx_spending(&sent.wallet.funding, 0, &[(theirs.script_pubkey(), 200_000)]);
        sent.harness.queue_transactions(from_ref(&elsewhere));
        sent.harness.queue_transactions(from_ref(&sent.wallet.funding));

        let bump = sent.harness.adapter.fee_bump().expect("the adapter can replace a transaction");
        let txid = TxId::new(params.chain, elsewhere.compute_txid().to_string());
        let error =
            bump.build_bump(&txid, &at(HIGHER)).await.expect_err("it is not ours to replace");
        assert!(error.to_string().contains("does not spend from this wallet"), "{error}");
    }
}

#[tokio::test]
async fn a_transaction_the_last_pass_saw_in_a_block_cannot_be_replaced() {
    for params in REPLACEABLE {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let txid = wallet.payment.compute_txid().to_string();
        harness.forget_calls();

        let bump = harness.adapter.fee_bump().expect("the adapter can replace a transaction");
        for value in [txid.clone(), txid.to_ascii_uppercase()] {
            let confirmed = TxId::new(params.chain, value);
            let error = bump
                .build_bump(&confirmed, &at(HIGHER))
                .await
                .expect_err("a confirmed send is refused");
            assert!(error.to_string().contains("already confirmed"), "{error}");
            let error = bump
                .quote_bump(&confirmed, &FeeSelection::NORMAL)
                .await
                .expect_err("no replacement quote");
            assert!(error.to_string().contains("already confirmed"), "{error}");
        }
        assert_eq!(
            harness.calls(Method::GetTransaction),
            0,
            "the refusal happens before anything is fetched"
        );
    }
}

#[tokio::test]
async fn a_transaction_id_that_is_not_one_is_refused_before_anything_is_fetched() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let bump = sent.harness.adapter.fee_bump().expect("the adapter can replace a transaction");
        let broken = TxId::new(params.chain, "not a transaction id");
        sent.harness.forget_calls();
        bump.quote_bump(&broken, &FeeSelection::NORMAL).await.unwrap_err();
        bump.build_bump(&broken, &at(HIGHER)).await.unwrap_err();
        assert_eq!(sent.harness.chain.server.calls(), Vec::new(), "{}", params.chain);
    }
}

/// The wallet spends its own unconfirmed change, so a second send can descend
/// from the first.
///
/// Replacing the first evicts the second, BIP-125 rule 3 makes the replacement
/// pay at least what both paid, and the preview says the second is cancelled.
#[tokio::test]
async fn a_replacement_pays_for_the_descendants_it_evicts() {
    let sent = sent(&BITCOIN).await.unwrap();
    let wallet = Wallet::new(&BITCOIN, ScriptKind::P2wpkh, &[FUNDING_SAT]).unwrap();
    let change = change_of(&sent.original).unwrap();
    let child_fee = 5_000u64;
    let stranger = sent.harness.key(ScriptKind::P2wpkh, 9).unwrap();
    let child = tx_spending(
        &sent.original,
        CHANGE_VOUT,
        &[(stranger.script_pubkey(), change.saturating_sub(child_fee))],
    );

    let restarted = Harness::new(Setup::on(&BITCOIN)).unwrap();
    Ledger::new()
        .known(&wallet.funding)
        .confirmed(&wallet.payment, MINED_AT)
        .mempool(&sent.original)
        .mempool(&child)
        .serve(&restarted);
    restarted.watch(&[wallet.address()]).await.unwrap();
    restarted.sync().await.unwrap();

    restarted.queue_transactions(from_ref(&sent.original));
    restarted.queue_transactions(from_ref(&wallet.payment));
    let bump = restarted.adapter.fee_bump().expect("the adapter can replace a transaction");
    let replacement = bump.build_bump(&sent.signed.txid, &at(HIGHER)).await.unwrap();
    let original_fee = payload(&sent.unsigned).unwrap().fee;
    let evicted = original_fee.checked_add(sats(child_fee)).expect("a total");
    let paid = payload(&replacement).unwrap().fee;
    assert!(
        paid > evicted,
        "the replacement pays {paid} but evicts transactions that paid {evicted} together"
    );
    assert!(
        replacement
            .preview
            .warnings
            .iter()
            .any(|warning| warning.message.contains("also cancels a later")),
        "{:?}",
        replacement.preview.warnings
    );
}

/// A replacement that adds a confirmed coin claims it alongside the original's
/// inputs, which are already spent in the mempool.
///
/// A pass that does not list those inputs must not free the added coin while
/// the replacement awaits broadcast.
#[tokio::test]
async fn a_pass_keeps_the_claim_on_a_coin_a_pending_replacement_added() {
    for params in REPLACEABLE {
        let (harness, wallet) =
            synced(Setup::on(&params), kind_of(&params), &[FUNDING_SAT]).await.unwrap();
        let everything =
            drain(&params, wallet.theirs.address(), FeeSelection::Rate { rate: "2".to_owned() });
        let unsigned = harness.adapter.build_tx(&everything).await.unwrap();
        let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();
        let original = decode(signed.raw.as_bytes()).unwrap();

        let stranger = harness.key(ScriptKind::P2wpkh, 9).unwrap();
        let elsewhere =
            tx_paying(&[(stranger.script_pubkey(), 900_000), (stranger.script_pubkey(), 1)]);
        let extra = tx_spending(&elsewhere, 0, &[(wallet.ours.script_pubkey(), 200_000)]);
        let added = bitcoin::OutPoint { txid: extra.compute_txid(), vout: 0 };
        let ledger = Ledger::new()
            .known(&wallet.funding)
            .known(&elsewhere)
            .confirmed(&wallet.payment, MINED_AT)
            .mempool(&original)
            .confirmed(&extra, MINED_LATER)
            .unspent(&extra, 0, 200_000, MINED_LATER);

        let restarted = Harness::new(Setup::on(&params)).unwrap();
        ledger.serve(&restarted);
        restarted.watch(&[wallet.address()]).await.unwrap();
        restarted.sync().await.unwrap();

        restarted.queue_transactions(from_ref(&original));
        restarted.queue_transactions(from_ref(&wallet.payment));
        let bump = restarted.adapter.fee_bump().expect("the adapter can replace a transaction");
        let replacement = bump.build_bump(&signed.txid, &at(HIGHER)).await.unwrap();
        let after = payload(&replacement).unwrap();
        assert!(
            after.tx.input.iter().any(|txin| txin.previous_output == added),
            "{}: the replacement needed the extra coin",
            params.chain
        );
        assert!(claimed(&restarted.adapter).contains(&added));

        // The server's answers stand, so this pass sees the same history.
        restarted.sync().await.unwrap();
        assert!(
            claimed(&restarted.adapter).contains(&added),
            "{}: a pass freed a coin the unbroadcast replacement spends",
            params.chain
        );
    }
}

/// Once its replacement is mined the original can never confirm, so the row the
/// engine recorded for it has to settle as replaced, the way an EVM send another
/// takes the nonce of settles.
#[tokio::test]
async fn a_replaced_original_is_reported_replaced_once_its_replacement_is_mined() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let change = change_of(&sent.original).unwrap();
        Ledger::new()
            .known(&sent.wallet.funding)
            .confirmed(&sent.wallet.payment, MINED_AT)
            .mempool(&sent.original)
            .unconfirmed(&sent.original, CHANGE_VOUT, change)
            .serve(&sent.harness);
        sent.harness.sync().await.unwrap();
        sent.harness.drain();

        let unsigned = sent.bump(HIGHER).await.unwrap();
        let signed =
            sent.harness.adapter.sign_tx(&unsigned, &[sent.wallet.ours.material()]).unwrap();
        let replacement = decode(signed.raw.as_bytes()).unwrap();
        let kept = change_of(&replacement).unwrap();
        Ledger::new()
            .known(&sent.wallet.funding)
            .confirmed(&sent.wallet.payment, MINED_AT)
            .confirmed(&replacement, MINED_LATER)
            .unspent(&replacement, CHANGE_VOUT, kept, MINED_LATER)
            .serve(&sent.harness);
        sent.harness.sync().await.unwrap();

        let original = sent.signed.txid.value().to_owned();
        let emitted = sent.harness.drain();
        let settled: Vec<TxOutcome> = records(&emitted)
            .into_iter()
            .filter(|record| record.txid.value() == original)
            .map(|record| record.outcome)
            .collect();
        assert_eq!(
            settled,
            vec![TxOutcome::Replaced],
            "{}: the replaced original is left pending forever",
            params.chain
        );
    }
}

#[tokio::test]
async fn a_payment_from_a_previous_session_can_still_be_replaced() {
    for params in REPLACEABLE {
        let sent = sent(&params).await.unwrap();
        let wallet = Wallet::new(&params, kind_of(&params), &[FUNDING_SAT]).unwrap();
        let change = change_of(&sent.original).unwrap();

        // A fresh adapter over the same address, which knows only what the server
        // tells it about the funding payment and the unconfirmed send.
        let restarted = Harness::new(Setup::on(&params)).unwrap();
        Ledger::new()
            .known(&wallet.funding)
            .confirmed(&wallet.payment, MINED_AT)
            .mempool(&sent.original)
            .unconfirmed(&sent.original, CHANGE_VOUT, change)
            .serve(&restarted);
        restarted.watch(&[wallet.address()]).await.unwrap();
        restarted.sync().await.unwrap();
        assert!(claimed(&restarted.adapter).is_empty(), "it holds no claim it did not make");

        restarted.queue_transactions(from_ref(&sent.original));
        restarted.queue_transactions(from_ref(&wallet.payment));
        let bump = restarted.adapter.fee_bump().expect("the adapter can replace a transaction");
        let replacement = bump.build_bump(&sent.signed.txid, &at(HIGHER)).await.unwrap();

        let after = payload(&replacement).unwrap();
        assert_eq!(after.to, wallet.theirs.address());
        assert_eq!(after.sources, [wallet.ours.address()]);
        assert_eq!(replacement.preview.amount, sats(PAYMENT_SAT));
        assert!(after.fee > payload(&sent.unsigned).unwrap().fee);
    }
}

/// The wallet's own later payment spends the original's change, so the replacement
/// evicts it, and the preview says it is cancelled.
///
/// Once the replacement is in, that payment's input no longer exists and it can
/// never confirm, so its row has to settle as replaced the way the original's does.
#[tokio::test]
async fn a_later_payment_the_mined_replacement_cancels_is_reported_replaced() {
    let sent = sent(&BITCOIN).await.unwrap();
    let change = change_of(&sent.original).unwrap();
    let stranger = sent.harness.key(ScriptKind::P2wpkh, 9).unwrap();
    let later = tx_spending(
        &sent.original,
        CHANGE_VOUT,
        &[(stranger.script_pubkey(), change.saturating_sub(5_000))],
    );
    Ledger::new()
        .known(&sent.wallet.funding)
        .confirmed(&sent.wallet.payment, MINED_AT)
        .mempool(&sent.original)
        .mempool(&later)
        .serve(&sent.harness);
    sent.harness.sync().await.unwrap();
    sent.harness.drain();

    let unsigned = sent.bump(HIGHER).await.unwrap();
    assert!(
        unsigned.preview.warnings.iter().any(|warning| warning.message.contains("also cancels")),
        "the preview tells the user the later payment is cancelled: {:?}",
        unsigned.preview.warnings
    );
    let signed = sent.harness.adapter.sign_tx(&unsigned, &[sent.wallet.ours.material()]).unwrap();
    let replacement = decode(signed.raw.as_bytes()).unwrap();
    let kept = change_of(&replacement).unwrap();
    Ledger::new()
        .known(&sent.wallet.funding)
        .confirmed(&sent.wallet.payment, MINED_AT)
        .confirmed(&replacement, MINED_LATER)
        .unspent(&replacement, CHANGE_VOUT, kept, MINED_LATER)
        .serve(&sent.harness);
    sent.harness.sync().await.unwrap();

    let cancelled = later.compute_txid().to_string();
    let emitted = sent.harness.drain();
    let settled: Vec<TxOutcome> = records(&emitted)
        .into_iter()
        .filter(|record| record.txid.value() == cancelled)
        .map(|record| record.outcome)
        .collect();
    assert_eq!(
        settled,
        vec![TxOutcome::Replaced],
        "the payment the mined replacement cancelled should settle as replaced, but the pass \
         reported {settled:?} for it, so its row stays pending forever"
    );
}

/// A broadcast send keeps its claim until a pass sees its coins spent.
///
/// Building a replacement takes that claim over, so backing out of the bump, which
/// the engine reports as a discard of the replacement, must leave the original's
/// coin claimed rather than free for the next payment.
#[tokio::test]
async fn discarding_a_replacement_keeps_the_coin_its_broadcast_original_spends_claimed() {
    let sent = sent(&BITCOIN).await.unwrap();
    sent.harness.serve(Method::Broadcast, &quoted(sent.signed.txid.value()));
    sent.harness.adapter.broadcast(&sent.signed).await.unwrap();
    let spent = sent.original.input.first().map(|txin| txin.previous_output).expect("an input");
    assert_eq!(claimed(&sent.harness.adapter), vec![spent], "the broadcast send holds its coin");

    let replacement = sent.bump(HIGHER).await.unwrap();
    let tracker = sent.harness.adapter.pending_tracker().expect("the adapter tracks pending sends");
    tracker.discard(&replacement).await.unwrap();
    let held = claimed(&sent.harness.adapter);

    // No pass has run since the broadcast, so the last one still lists the coin.
    let next = sent
        .harness
        .adapter
        .build_tx(&sent.harness.request(sent.wallet.theirs.address(), 50_000, RATE))
        .await;
    let is_reused = next.as_ref().is_ok_and(|unsigned| {
        payload(unsigned)
            .is_ok_and(|after| after.tx.input.iter().any(|txin| txin.previous_output == spent))
    });
    assert!(
        !is_reused,
        "the broadcast original's coin {spent} should stay claimed, but discarding the \
         replacement left the claims at {held:?} and a new payment spends the coin again"
    );
}

/// A replacement the wallet broadcast just before it closed is mined while it is closed.
///
/// The next session's first pass is the first to see the original gone, and the
/// original can still never confirm, so its row has to settle as replaced.
#[tokio::test]
async fn an_original_replaced_before_a_restart_is_reported_replaced_by_the_next_session() {
    let sent = sent(&BITCOIN).await.unwrap();
    let change = change_of(&sent.original).unwrap();
    Ledger::new()
        .known(&sent.wallet.funding)
        .confirmed(&sent.wallet.payment, MINED_AT)
        .mempool(&sent.original)
        .unconfirmed(&sent.original, CHANGE_VOUT, change)
        .serve(&sent.harness);
    sent.harness.sync().await.unwrap();

    let unsigned = sent.bump(HIGHER).await.unwrap();
    let signed = sent.harness.adapter.sign_tx(&unsigned, &[sent.wallet.ours.material()]).unwrap();
    let replacement = decode(signed.raw.as_bytes()).unwrap();
    let kept = change_of(&replacement).unwrap();

    let restarted = Harness::over(Setup::on(&BITCOIN), Arc::clone(&sent.harness.cursors)).unwrap();
    Ledger::new()
        .known(&sent.wallet.funding)
        .confirmed(&sent.wallet.payment, MINED_AT)
        .confirmed(&replacement, MINED_LATER)
        .unspent(&replacement, CHANGE_VOUT, kept, MINED_LATER)
        .serve(&restarted);
    restarted.watch(&[sent.wallet.address()]).await.unwrap();
    restarted.sync().await.unwrap();

    let original = sent.signed.txid.value().to_owned();
    let emitted = restarted.drain();
    let settled: Vec<TxOutcome> = records(&emitted)
        .into_iter()
        .filter(|record| record.txid.value() == original)
        .map(|record| record.outcome)
        .collect();
    assert_eq!(
        settled,
        vec![TxOutcome::Replaced],
        "the original the mined replacement displaced should settle as replaced, but the first \
         pass after the restart reported {settled:?} for it, so its row stays pending forever"
    );
}
