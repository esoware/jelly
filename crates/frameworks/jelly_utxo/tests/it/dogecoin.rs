//! The flows the sat-scale suites run, at Dogecoin's own scale.
//!
//! A koinu is worth so little that the floors are three orders of magnitude higher.
//! The dust rule is a fixed value, headers are merge-mined, the one fee level is the
//! recommended rate, and a coinbase output waits 240 blocks.

use std::slice::from_ref;

use bitcoin::transaction::Version;
use jelly_core::{
    Amount, AssetId, ChainAdapter as _, ChainError, FeeRate, FeeSelection, ScriptKind,
    TxWarningKind,
    test_util::{TxRequestBuilder, records},
};
use jelly_electrum::Method;

use crate::common::{
    CHANGE_VOUT, DOGECOIN, Harness, Ledger, Setup, TIP, Wallet, coinbase_paying, decode,
    header_vector, payload, quoted, sats, synced, watched,
};

/// Ten DOGE.
const FUNDING_KOINU: u64 = 1_000_000_000;
/// One DOGE.
const PAYMENT_KOINU: u64 = 100_000_000;
/// The soft dust limit, 0.01 DOGE.
const DUST_KOINU: u64 = 1_000_000;
/// What a one-input, two-output P2PKH spend of 226 bytes pays at the recommended
/// 1000 koinu per byte.
const FEE_KOINU: u64 = 226_000;

const KIND: ScriptKind = ScriptKind::P2pkh;

fn rate(rate: FeeRate) -> String {
    rate.format_sat_per_vb()
}

#[tokio::test]
async fn a_wallet_syncs_through_merge_mined_headers() {
    let harness = Harness::new(Setup::on(&DOGECOIN)).unwrap();
    let wallet = Wallet::new(&DOGECOIN, KIND, &[FUNDING_KOINU]).unwrap();
    wallet.ledger().serve(&harness);
    let merge_mined = header_vector("dogecoin5000000").unwrap();
    harness
        .serve(Method::HeadersSubscribe, &format!(r#"{{"height":{TIP},"hex":"{merge_mined}"}}"#));
    harness.serve(Method::BlockHeader, &quoted(&merge_mined));
    harness.watch(&[wallet.address()]).await.unwrap();

    let progress = harness.sync().await.unwrap();
    assert_eq!(progress.tip_height(), Some(u64::from(TIP)));
    let emitted = harness.drain();
    let record = records(&emitted).into_iter().next().expect("the payment is recorded");
    assert_eq!(record.amount, sats(FUNDING_KOINU));
    assert_eq!(record.timestamp, Some(1_702_188_235), "the time the merge-mined header carries");
}

/// Dogecoin Core's wallet pays the recommended 0.01 DOGE/kB, and the servers'
/// estimators are never asked, because they swing by two orders of magnitude.
#[tokio::test]
async fn the_normal_level_pays_the_recommended_rate_without_asking_a_server() {
    let (harness, wallet) = synced(Setup::on(&DOGECOIN), KIND, &[FUNDING_KOINU]).await.unwrap();
    harness.serve(Method::EstimateFee, "1.63426479");
    harness.forget_calls();
    let request = TxRequestBuilder::new(
        7,
        wallet.theirs.address(),
        AssetId::Native(DOGECOIN.chain),
        sats(PAYMENT_KOINU),
    )
    .fee(FeeSelection::NORMAL)
    .build();
    let quote = harness.adapter.quote_fee(&request).await.unwrap();
    assert_eq!(quote.custom, Some(FeeSelection::Rate { rate: "1000".to_owned() }));
    for method in [Method::MempoolInfo, Method::EstimateFee, Method::FeeHistogram] {
        assert_eq!(harness.calls(method), 0, "{method}");
    }

    let unsigned = harness.adapter.build_tx(&request).await.unwrap();
    let payload = payload(&unsigned).unwrap();
    assert_eq!(unsigned.preview.fee, payload.fee);
    assert_eq!(payload.fee, sats(FEE_KOINU));
    let detail = |label: &str| {
        unsigned.preview.details.iter().find(|row| row.label == label).map(|row| row.value.as_str())
    };
    assert_eq!(detail("Fee rate"), Some("1000 koinu/B"));
    assert_eq!(detail("Size"), Some("226 bytes"));
}

/// A node relays down to a tenth of the recommended rate and miners include from
/// the recommended rate, so anything between is relayed but slow, and anything
/// below is refused before it's built.
#[tokio::test]
async fn a_rate_below_the_recommendation_is_slow_and_below_the_relay_floor_is_refused() {
    let (harness, wallet) = synced(Setup::on(&DOGECOIN), KIND, &[FUNDING_KOINU]).await.unwrap();
    let slow = harness
        .adapter
        .build_tx(&harness.request(
            wallet.theirs.address(),
            PAYMENT_KOINU,
            &rate(DOGECOIN.min_relay_fee),
        ))
        .await
        .unwrap();
    assert!(
        slow.preview.warnings.iter().any(|warning| warning.message.contains("long time")),
        "{:?}",
        slow.preview.warnings
    );
    harness.adapter.pending_tracker().expect("pending").discard(&slow).await.unwrap();

    let error = harness
        .adapter
        .build_tx(&harness.request(wallet.theirs.address(), PAYMENT_KOINU, "50"))
        .await
        .expect_err("half the relay floor");
    assert!(error.to_string().contains("koinu/B"), "{error}");
}

/// Below 0.01 DOGE a node charges another 0.01 DOGE per output, so the reference
/// wallet never writes one. A payment that small is refused, and change that small
/// goes to the fee.
#[tokio::test]
async fn an_output_below_the_soft_dust_limit_is_never_written() {
    let (harness, wallet) = synced(Setup::on(&DOGECOIN), KIND, &[FUNDING_KOINU]).await.unwrap();
    let error = harness
        .adapter
        .build_tx(&harness.request(wallet.theirs.address(), DUST_KOINU - 1, "1000"))
        .await
        .expect_err("below the soft dust limit");
    assert!(error.to_string().contains("0.01 DOGE"), "{error}");
    harness
        .adapter
        .build_tx(&harness.request(wallet.theirs.address(), DUST_KOINU, "1000"))
        .await
        .expect("exactly the soft dust limit");

    // Leave half the dust limit as change, which no node would take for free.
    let (harness, wallet) = synced(Setup::on(&DOGECOIN), KIND, &[FUNDING_KOINU]).await.unwrap();
    let almost_all = FUNDING_KOINU - FEE_KOINU - DUST_KOINU.div_euclid(2);
    let unsigned = harness
        .adapter
        .build_tx(&harness.request(wallet.theirs.address(), almost_all, "1000"))
        .await
        .unwrap();
    let payload = payload(&unsigned).unwrap();
    assert_eq!(payload.tx.output.len(), 1, "no change output");
    assert!(payload.donated > Amount::ZERO);
    assert!(
        unsigned
            .preview
            .warnings
            .iter()
            .any(|warning| warning.kind == TxWarningKind::DroppedChange),
        "{:?}",
        unsigned.preview.warnings
    );
}

/// Dogecoin Core relays BIP-125 replacements by default, so a stuck payment can be
/// bumped, and the replacement keeps paying the same recipient.
#[tokio::test]
async fn a_dogecoin_payment_can_be_replaced_at_a_higher_rate() {
    let (harness, wallet) = synced(Setup::on(&DOGECOIN), KIND, &[FUNDING_KOINU]).await.unwrap();
    let unsigned = harness
        .adapter
        .build_tx(&harness.request(
            wallet.theirs.address(),
            PAYMENT_KOINU,
            &rate(DOGECOIN.min_relay_fee),
        ))
        .await
        .unwrap();
    let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()]).unwrap();
    let original = decode(signed.raw.as_bytes()).unwrap();
    assert!(original.input.iter().all(|txin| txin.sequence.is_rbf()));

    harness.queue_transactions(from_ref(&original));
    harness.queue_transactions(from_ref(&wallet.payment));
    let bump = harness.adapter.fee_bump().expect("dogecoin replaces");
    let replacement = bump.build_bump(&signed.txid, &FeeSelection::NORMAL).await.unwrap();
    let after = payload(&replacement).unwrap();
    let before = payload(&unsigned).unwrap();
    assert_eq!(after.replaces.map(|txid| txid.to_string()).as_deref(), Some(signed.txid.value()));
    assert_eq!(after.to, wallet.theirs.address());
    assert_eq!(replacement.preview.amount, sats(PAYMENT_KOINU));
    assert!(after.fee > before.fee);
    assert!(after.tx.output.get(usize::try_from(CHANGE_VOUT).unwrap()).is_some());
    // A replacement at the normal level pays the recommended rate, which miners include.
    assert!(after.fee >= sats(FEE_KOINU), "{}", after.fee);
}

/// A coinbase output waits 240 blocks on Dogecoin, not Bitcoin's hundred.
#[tokio::test]
async fn a_coinbase_output_waits_two_hundred_forty_blocks() {
    for (confirmations, is_spendable) in [(150u32, false), (239, false), (240, true)] {
        let harness = Harness::new(Setup::on(&DOGECOIN)).unwrap();
        let ours = harness.key(KIND, 1).unwrap();
        let theirs = harness.key(KIND, 2).unwrap();
        let coinbase = coinbase_paying(ours.script_pubkey(), FUNDING_KOINU, Version::ONE);
        let mined = TIP.saturating_add(1).saturating_sub(confirmations);
        Ledger::new()
            .confirmed(&coinbase, mined)
            .unspent(&coinbase, 0, FUNDING_KOINU, mined)
            .serve(&harness);
        harness.watch(&[watched(&DOGECOIN, 7, ours.address())]).await.unwrap();
        harness.sync().await.unwrap();

        let built = harness
            .adapter
            .build_tx(&harness.request(theirs.address(), PAYMENT_KOINU, "1000"))
            .await;
        match built {
            Ok(_) => assert!(is_spendable, "{confirmations}"),
            Err(error) => {
                assert!(!is_spendable, "{confirmations}: {error}");
                assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error}");
            }
        }
    }
}
