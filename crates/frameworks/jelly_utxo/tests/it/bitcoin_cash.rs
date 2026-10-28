//! What Bitcoin Cash does that the other tables don't.
//!
//! Its outputs can carry `CashTokens` or SLP tokens, its legacy address forms are
//! Bitcoin's too, a checkpoint tells its servers from Bitcoin's, its signatures are
//! replay protected, and its fee asks no server.

use std::sync::Arc;

use bitcoin::{ScriptBuf, Transaction};
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, FeeLevel, FeeRate, FeeSelection, ScriptKind,
    TxDirection, TxWarningKind, WalletEvent,
    test_util::{MemoryCursorStore, TxRequestBuilder, records},
};
use jelly_electrum::Method;
use jelly_utxo::{
    UtxoParams,
    test_util::{TestKey, test_key, tx_paying, tx_spending},
};

use crate::common::{
    BITCOIN, BITCOIN_CASH, BLOCK_UNIX_SECONDS, FakeFeeApi, Fallible, Harness, Ledger, MINED_AT,
    Setup, TIP, checkpoint_header, decode, drain, every_level_at, payload, sats, synced, watched,
};

const FUNDING_SAT: u64 = 500_000;
const PAYMENT_SAT: u64 = 100_000;
/// What a token output typically carries, the dust a token wallet funds it with.
const TOKEN_DUST_SAT: u64 = 800;

/// `script` behind a prefix carrying one hundred fungible tokens of a made-up
/// category.
fn with_tokens(script: &ScriptBuf) -> ScriptBuf {
    let mut bytes = vec![0xef];
    bytes.extend_from_slice(&[0xbb; 32]);
    bytes.extend_from_slice(&[0x10, 0x64]);
    bytes.extend_from_slice(script.as_bytes());
    ScriptBuf::from(bytes)
}

fn balances(events: &[WalletEvent]) -> Vec<Amount> {
    events
        .iter()
        .filter_map(|event| match *event {
            WalletEvent::BalanceChanged(ref update) => Some(update.balance),
            WalletEvent::TxDetected(_)
            | WalletEvent::TxUpdated(_)
            | WalletEvent::TxConfirmed(_)
            | WalletEvent::SyncProgress(_) => None,
        })
        .collect()
}

/// A wallet whose one payment brought a plain coin and a coin that carries
/// tokens, the way a token wallet funds a token output.
struct Holding {
    harness: Harness,
    ours: TestKey,
    theirs: TestKey,
    payment: Transaction,
}

async fn holding(cursors: Arc<MemoryCursorStore>) -> Fallible<Holding> {
    let harness = Harness::over(Setup::on(&BITCOIN_CASH), cursors)?;
    let ours = harness.key(ScriptKind::P2pkh, 1)?;
    let theirs = harness.key(ScriptKind::P2pkh, 2)?;
    let funding = tx_paying(&[(theirs.script_pubkey(), FUNDING_SAT + TOKEN_DUST_SAT + 1_000)]);
    let payment = tx_spending(
        &funding,
        0,
        &[
            (ours.script_pubkey(), FUNDING_SAT),
            (with_tokens(&ours.script_pubkey()), TOKEN_DUST_SAT),
        ],
    );
    Ledger::new()
        .known(&funding)
        .confirmed(&payment, MINED_AT)
        .unspent(&payment, 0, FUNDING_SAT, MINED_AT)
        .unspent(&payment, 1, TOKEN_DUST_SAT, MINED_AT)
        .serve(&harness);
    harness.watch(&[watched(&BITCOIN_CASH, 7, ours.address())]).await?;
    Ok(Holding { harness, ours, theirs, payment })
}

/// Spending a coin that carries tokens into plain outputs burns the tokens, so it
/// counts toward the balance, as an immature coinbase does, and is never an
/// input.
#[tokio::test]
async fn a_coin_that_carries_tokens_counts_toward_the_balance_and_is_never_spent() {
    let Holding { harness, theirs, payment, .. } =
        holding(Arc::new(MemoryCursorStore::new())).await.unwrap();
    harness.sync().await.unwrap();
    let emitted = harness.drain();
    assert_eq!(balances(&emitted), [sats(FUNDING_SAT + TOKEN_DUST_SAT)]);
    let record = records(&emitted).into_iter().next().expect("the payment is recorded");
    assert_eq!(record.txid.value(), payment.compute_txid().to_string());
    assert_eq!(record.direction, TxDirection::Incoming);
    assert_eq!(
        record.amount,
        sats(FUNDING_SAT + TOKEN_DUST_SAT),
        "the history adds up to the balance"
    );

    let unsigned = harness
        .adapter
        .build_tx(&drain(
            &BITCOIN_CASH,
            theirs.address(),
            FeeSelection::Rate { rate: "1".to_owned() },
        ))
        .await
        .unwrap();
    let decoded = payload(&unsigned).unwrap();
    assert_eq!(decoded.tx.input.len(), 1);
    assert_eq!(
        decoded.tx.input.first().map(|txin| txin.previous_output.vout),
        Some(0),
        "only the plain coin leaves"
    );
    assert_eq!(unsigned.preview.amount.checked_add(unsigned.preview.fee), Some(sats(FUNDING_SAT)));

    let error = harness
        .adapter
        .build_tx(&harness.request(theirs.address(), FUNDING_SAT, "1"))
        .await
        .expect_err("the plain coin cannot cover it and the token coin is held");
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error}");
}

/// The flag lives in the cursor, so a build before a later session's first pass
/// still leaves the token coin alone.
#[tokio::test]
async fn a_later_session_keeps_a_coin_with_tokens_out_of_a_build() {
    let cursors = Arc::new(MemoryCursorStore::new());
    let first = holding(Arc::clone(&cursors)).await.unwrap();
    first.harness.sync().await.unwrap();

    let later = Harness::over(Setup::on(&BITCOIN_CASH), cursors).unwrap();
    later.watch(&[watched(&BITCOIN_CASH, 7, first.ours.address())]).await.unwrap();
    later.chain.tip(TIP, BLOCK_UNIX_SECONDS);
    let unsigned = later
        .adapter
        .build_tx(&drain(
            &BITCOIN_CASH,
            first.theirs.address(),
            FeeSelection::Rate { rate: "1".to_owned() },
        ))
        .await
        .unwrap();
    let decoded = payload(&unsigned).unwrap();
    assert_eq!(
        decoded.tx.input.iter().map(|txin| txin.previous_output.vout).collect::<Vec<_>>(),
        [0]
    );
}

/// On a chain without tokens the prefixed output isn't the address's script, so it
/// doesn't count even when a server lists it.
#[tokio::test]
async fn a_token_prefix_means_nothing_on_a_chain_without_tokens() {
    let harness = Harness::new(Setup::on(&BITCOIN)).unwrap();
    let ours = harness.key(ScriptKind::P2wpkh, 1).unwrap();
    let theirs = harness.key(ScriptKind::P2wpkh, 2).unwrap();
    let funding = tx_paying(&[(theirs.script_pubkey(), FUNDING_SAT + TOKEN_DUST_SAT + 1_000)]);
    let payment = tx_spending(
        &funding,
        0,
        &[
            (ours.script_pubkey(), FUNDING_SAT),
            (with_tokens(&ours.script_pubkey()), TOKEN_DUST_SAT),
        ],
    );
    Ledger::new()
        .known(&funding)
        .confirmed(&payment, MINED_AT)
        .unspent(&payment, 0, FUNDING_SAT, MINED_AT)
        .unspent(&payment, 1, TOKEN_DUST_SAT, MINED_AT)
        .serve(&harness);
    harness.watch(&[watched(&BITCOIN, 7, ours.address())]).await.unwrap();
    harness.sync().await.unwrap();
    assert_eq!(balances(&harness.drain()), [sats(FUNDING_SAT)]);
}

/// An SLP SEND of a made-up token to output 1, with `quantity` written in
/// `quantity.len()` bytes, which a well-formed message writes in eight.
fn slp_send(quantity: &[u8]) -> Fallible<ScriptBuf> {
    let mut bytes = vec![0x6a, 0x04];
    bytes.extend_from_slice(b"SLP\x00");
    bytes.extend_from_slice(&[0x01, 0x01, 0x04]);
    bytes.extend_from_slice(b"SEND");
    bytes.push(0x20);
    bytes.extend_from_slice(&[0xbb; 32]);
    bytes.push(u8::try_from(quantity.len())?);
    bytes.extend_from_slice(quantity);
    Ok(ScriptBuf::from(bytes))
}

/// A payment whose first output is `message`, which pays ours a token dust output
/// and then a plain coin, as an SLP wallet sends tokens with change.
async fn slp_holding(params: &UtxoParams, message: ScriptBuf) -> Fallible<Holding> {
    let harness = Harness::new(Setup::on(params))?;
    let ours = harness.key(ScriptKind::P2pkh, 1)?;
    let theirs = harness.key(ScriptKind::P2pkh, 2)?;
    let funding = tx_paying(&[(theirs.script_pubkey(), FUNDING_SAT + TOKEN_DUST_SAT + 1_000)]);
    let payment = tx_spending(
        &funding,
        0,
        &[
            (message, 0),
            (ours.script_pubkey(), TOKEN_DUST_SAT),
            (ours.script_pubkey(), FUNDING_SAT),
        ],
    );
    Ledger::new()
        .known(&funding)
        .confirmed(&payment, MINED_AT)
        .unspent(&payment, 1, TOKEN_DUST_SAT, MINED_AT)
        .unspent(&payment, 2, FUNDING_SAT, MINED_AT)
        .serve(&harness);
    harness.watch(&[watched(params, 7, ours.address())]).await?;
    Ok(Holding { harness, ours, theirs, payment })
}

/// The vouts a send-max from `holding` spends, in order.
async fn drained_vouts(holding: &Holding, params: &UtxoParams) -> Fallible<Vec<u32>> {
    let request =
        drain(params, holding.theirs.address(), FeeSelection::Rate { rate: "1".to_owned() });
    let unsigned = holding.harness.adapter.build_tx(&request).await?;
    let decoded = payload(&unsigned)?;
    let mut vouts: Vec<u32> =
        decoded.tx.input.iter().map(|txin| txin.previous_output.vout).collect();
    vouts.sort_unstable();
    Ok(vouts)
}

/// An SLP coin is a plain output whose tokens only its transaction's first output
/// names. Like a `CashTokens` coin, it counts toward the balance and is never spent.
#[tokio::test]
async fn a_coin_an_slp_message_names_counts_toward_the_balance_and_is_never_spent() {
    let holding =
        slp_holding(&BITCOIN_CASH, slp_send(&100u64.to_be_bytes()).unwrap()).await.unwrap();
    holding.harness.sync().await.unwrap();
    let emitted = holding.harness.drain();
    assert_eq!(balances(&emitted), [sats(FUNDING_SAT + TOKEN_DUST_SAT)]);
    let record = records(&emitted).into_iter().next().expect("the payment is recorded");
    assert_eq!(record.txid.value(), holding.payment.compute_txid().to_string());
    assert_eq!(
        record.amount,
        sats(FUNDING_SAT + TOKEN_DUST_SAT),
        "the history adds up to the balance"
    );
    assert_eq!(
        drained_vouts(&holding, &BITCOIN_CASH).await.unwrap(),
        [2],
        "only the plain coin leaves"
    );

    let error = holding
        .harness
        .adapter
        .build_tx(&holding.harness.request(holding.theirs.address(), FUNDING_SAT, "1"))
        .await
        .expect_err("the plain coin cannot cover it and the token coin is held");
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error}");
}

/// The specification gives a malformed message no tokens, so nothing is held, and
/// on a chain without SLP the same message is an ordinary data output.
#[tokio::test]
async fn a_malformed_slp_message_or_a_chain_without_slp_holds_nothing() {
    let malformed =
        slp_holding(&BITCOIN_CASH, slp_send(&[0, 0, 0, 0, 0, 0, 100]).unwrap()).await.unwrap();
    malformed.harness.sync().await.unwrap();
    assert_eq!(drained_vouts(&malformed, &BITCOIN_CASH).await.unwrap(), [1, 2]);

    let bitcoin = slp_holding(&BITCOIN, slp_send(&100u64.to_be_bytes()).unwrap()).await.unwrap();
    bitcoin.harness.sync().await.unwrap();
    assert_eq!(drained_vouts(&bitcoin, &BITCOIN).await.unwrap(), [1, 2]);
}

/// Bitcoin Cash kept Bitcoin's version bytes, so a legacy address can't say which
/// chain its owner watches. A `CashAddr` can, and a token-aware one pays the plain
/// script.
#[tokio::test]
async fn a_legacy_recipient_is_paid_with_a_warning_and_cashaddr_without_one() {
    let stranger = test_key(&BITCOIN, ScriptKind::P2pkh, 9).unwrap();
    let legacy = stranger.address().to_owned();
    assert!(legacy.starts_with('1'), "{legacy}");
    let script = BITCOIN_CASH.script_pubkey(&legacy).unwrap();
    let cashaddr = BITCOIN_CASH.address_of(script.as_script()).expect("a cashaddr");
    let token_aware = "bitcoincash:zp63uahgrxged4z5jswyt5dn5v3lzsem6crlrlr74y";

    for (recipient, is_warned) in
        [(legacy.as_str(), true), (cashaddr.as_str(), false), (token_aware, false)]
    {
        let (harness, _wallet) =
            synced(Setup::on(&BITCOIN_CASH), ScriptKind::P2pkh, &[FUNDING_SAT]).await.unwrap();
        let unsigned =
            harness.adapter.build_tx(&harness.request(recipient, PAYMENT_SAT, "1")).await.unwrap();
        assert_eq!(
            unsigned
                .preview
                .warnings
                .iter()
                .any(|warning| warning.kind == TxWarningKind::AmbiguousAddress),
            is_warned,
            "{recipient}: {:?}",
            unsigned.preview.warnings
        );
        let expected = BITCOIN_CASH.script_pubkey(recipient).unwrap();
        let payload = payload(&unsigned).unwrap();
        assert!(payload.tx.output.iter().any(|txout| txout.script_pubkey == expected));
    }
}

/// Bitcoin and Bitcoin Cash share block zero, so a server proves the chain at the
/// checkpoint, and each table refuses the other's server there.
#[tokio::test]
async fn bitcoins_and_bitcoin_cashs_servers_are_told_apart_at_the_checkpoint() {
    for (params, other) in [(BITCOIN, BITCOIN_CASH), (BITCOIN_CASH, BITCOIN)] {
        let setup = Setup { checkpoint_hex: checkpoint_header(&other), ..Setup::on(&params) };
        assert_eq!(setup.genesis_hex, Setup::on(&other).genesis_hex, "block zero agrees");
        let harness = Harness::new(setup).unwrap();
        let key = harness.key(ScriptKind::P2pkh, 1).unwrap();
        Ledger::new().serve(&harness);
        harness.watch(&[watched(&params, 7, key.address())]).await.unwrap();
        let error = harness.sync().await.expect_err("a server on the other chain");
        assert!(matches!(error, ChainError::EndpointsDown { .. }), "{}: {error}", params.chain);
        assert_eq!(harness.calls(Method::GetHistory), 0, "nothing was asked of it");
    }
}

/// Every signature commits to `SIGHASH_FORKID`, and no input signals a
/// replacement no node would honor.
#[tokio::test]
async fn a_bitcoin_cash_payment_is_replay_protected_and_final_to_replacement() {
    let (harness, wallet) =
        synced(Setup::on(&BITCOIN_CASH), ScriptKind::P2pkh, &[FUNDING_SAT]).await.unwrap();
    assert!(harness.adapter.fee_bump().is_none());
    let unsigned = harness
        .adapter
        .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "1"))
        .await
        .unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();
    let tx = decode(signed.raw.as_bytes()).unwrap();
    assert_eq!(tx.compute_txid().to_string(), signed.txid.value());
    for txin in &tx.input {
        assert!(!txin.sequence.is_rbf());
        assert!(txin.witness.is_empty());
        let signature = txin
            .script_sig
            .instructions()
            .next()
            .and_then(Result::ok)
            .and_then(|instruction| instruction.push_bytes().map(|bytes| bytes.as_bytes().to_vec()))
            .expect("a signature push");
        assert_eq!(signature.last(), Some(&0x41), "SIGHASH_ALL with SIGHASH_FORKID");
    }
}

/// The one level is the relay floor whatever a server or a fee service says, and no
/// one is asked.
#[tokio::test]
async fn the_normal_level_pays_the_floor_without_asking_anyone() {
    let api =
        Arc::new(FakeFeeApi::answering(every_level_at(FeeRate::from_sat_per_kwu(25_000), None)));
    let (harness, wallet) = synced(
        Setup::with_fees(&BITCOIN_CASH, Arc::clone(&api)),
        ScriptKind::P2pkh,
        &[FUNDING_SAT],
    )
    .await
    .unwrap();
    harness.serve(Method::EstimateFee, "0.001");
    harness.forget_calls();
    let quote = harness
        .adapter
        .quote_fee(
            &TxRequestBuilder::new(
                7,
                wallet.theirs.address(),
                AssetId::Native(BITCOIN_CASH.chain),
                sats(PAYMENT_SAT),
            )
            .fee(FeeSelection::NORMAL)
            .build(),
        )
        .await
        .unwrap();
    assert_eq!(quote.custom, Some(FeeSelection::Rate { rate: "1".to_owned() }));
    let levels: Vec<FeeLevel> = quote.levels.iter().map(|row| row.level).collect();
    assert_eq!(levels, [FeeLevel::Normal], "a flat market offers only the normal level");
    for method in [Method::MempoolInfo, Method::EstimateFee, Method::FeeHistogram] {
        assert_eq!(harness.calls(method), 0, "{method}");
    }
    assert_eq!(api.calls(), 0);

    let error = harness
        .adapter
        .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_SAT, "0.5"))
        .await
        .expect_err("half the relay floor");
    assert!(error.to_string().contains("sat/B"), "{error}");
}
