//! Sending from several addresses at once.
//!
//! The coins of every source pool into one selection, each input is signed by its
//! own address's key, change returns to the first source, and a replacement keeps
//! every input and adds coins only from the sources of the transaction it
//! replaces.

use bitcoin::{OutPoint, Transaction};
use jelly_core::{
    AssetId, ChainAdapter as _, ChainError, FeeRate, FeeSelection, KeyMaterial, ScriptKind,
    TxRequest, UnsignedTx, test_util::TxRequestBuilder,
};
use jelly_utxo::test_util::{TestKey, claimed, tx_paying, tx_spending_each};

use crate::common::{
    BITCOIN, CHANGE_VOUT, Fallible, Harness, MINED_AT, ScriptLedger, Setup, at, decode, payload,
    sats, watched,
};

/// Above Bitcoin's 10 sat/vB long-term rate, so a build adds no coin it does not
/// need.
const RATE: &str = "12";
const PAYMENT_SAT: u64 = 120_000;

const NATIVE: i64 = 7;
const TAPROOT: i64 = 8;
const LEGACY: i64 = 9;
const EMPTY: i64 = 10;

/// Four addresses of one wallet: a native segwit one holding 60 000, a taproot
/// one holding 70 000 and 50 000, a legacy one holding 500 000, and one holding
/// nothing, beside a stranger.
struct Wallet {
    harness: Harness,
    native: TestKey,
    taproot: TestKey,
    legacy: TestKey,
    empty: TestKey,
    theirs: TestKey,
    payment: Transaction,
    ledger: ScriptLedger,
}

impl Wallet {
    const fn keys(&self) -> [&TestKey; 4] {
        [&self.native, &self.taproot, &self.legacy, &self.empty]
    }

    fn coin(&self, vout: u32) -> OutPoint {
        OutPoint { txid: self.payment.compute_txid(), vout }
    }

    fn send(&self, sources: &[i64], amount: u64) -> TxRequest {
        TxRequestBuilder::new(
            NATIVE,
            self.theirs.address(),
            AssetId::Native(BITCOIN.chain),
            sats(amount),
        )
        .sources(sources)
        .fee(FeeSelection::Rate { rate: RATE.to_owned() })
        .build()
    }

    fn key_of(&self, address: &str) -> Fallible<KeyMaterial> {
        self.keys()
            .into_iter()
            .find(|key| key.address() == address)
            .map(TestKey::material)
            .ok_or_else(|| format!("{address} is none of ours").into())
    }

    /// The keys of `unsigned`'s sources, in the order the preview names them.
    fn keys_for(&self, unsigned: &UnsignedTx) -> Fallible<Vec<KeyMaterial>> {
        unsigned.preview.from.iter().map(|address| self.key_of(address)).collect()
    }
}

async fn wallet() -> Fallible<Wallet> {
    let harness = Harness::new(Setup::on(&BITCOIN))?;
    let native = harness.key(ScriptKind::P2wpkh, 1)?;
    let theirs = harness.key(ScriptKind::P2wpkh, 2)?;
    let taproot = harness.key(ScriptKind::P2tr, 3)?;
    let legacy = harness.key(ScriptKind::P2pkh, 4)?;
    let empty = harness.key(ScriptKind::P2wpkh, 5)?;
    let funding = tx_paying(&[(theirs.script_pubkey(), 1_000_000)]);
    let payment = tx_spending_each(
        &[OutPoint { txid: funding.compute_txid(), vout: 0 }],
        &[
            (native.script_pubkey(), 60_000),
            (taproot.script_pubkey(), 70_000),
            (taproot.script_pubkey(), 50_000),
            (legacy.script_pubkey(), 500_000),
        ],
    );
    let ledger = ScriptLedger::new()
        .known(&funding)
        .confirmed(&payment, MINED_AT, &[&native, &taproot, &legacy])
        .unspent(&payment, 0, MINED_AT)
        .unspent(&payment, 1, MINED_AT)
        .unspent(&payment, 2, MINED_AT)
        .unspent(&payment, 3, MINED_AT);
    let wallet = Wallet { harness, native, taproot, legacy, empty, theirs, payment, ledger };
    wallet.ledger.serve(&wallet.harness, &wallet.keys());
    wallet
        .harness
        .watch(&[
            watched(&BITCOIN, NATIVE, wallet.native.address()),
            watched(&BITCOIN, TAPROOT, wallet.taproot.address()),
            watched(&BITCOIN, LEGACY, wallet.legacy.address()),
            watched(&BITCOIN, EMPTY, wallet.empty.address()),
        ])
        .await?;
    wallet.harness.sync().await?;
    wallet.harness.drain();
    Ok(wallet)
}

fn spent(unsigned: &UnsignedTx) -> Fallible<Vec<OutPoint>> {
    let mut spent: Vec<OutPoint> =
        payload(unsigned)?.tx.input.iter().map(|txin| txin.previous_output).collect();
    spent.sort();
    Ok(spent)
}

/// Neither address alone pays, so the build takes a coin of each, signs each with
/// its own key, and returns the change to the first. The legacy address the
/// request did not name is never touched.
#[tokio::test]
async fn a_send_pools_the_coins_of_its_sources_and_returns_change_to_the_first() {
    let wallet = wallet().await.unwrap();
    let unsigned = wallet
        .harness
        .adapter
        .build_tx(&wallet.send(&[NATIVE, TAPROOT], PAYMENT_SAT))
        .await
        .unwrap();

    assert_eq!(
        unsigned.preview.from,
        [wallet.native.address().to_owned(), wallet.taproot.address().to_owned()]
    );
    let mut expected = vec![wallet.coin(0), wallet.coin(1)];
    expected.sort();
    assert_eq!(spent(&unsigned).unwrap(), expected);
    assert_eq!(claimed(&wallet.harness.adapter), expected, "every input is claimed");
    let payload = payload(&unsigned).unwrap();
    let change = &payload.tx.output[usize::try_from(CHANGE_VOUT).unwrap()];
    assert_eq!(change.script_pubkey, wallet.native.script_pubkey());
    assert_eq!(unsigned.preview.amount, sats(PAYMENT_SAT));

    let keys = wallet.keys_for(&unsigned).unwrap();
    let signed = wallet.harness.adapter.sign_tx(&unsigned, &keys).unwrap();
    let tx = decode(signed.raw.as_bytes()).unwrap();
    assert_eq!(tx.compute_txid().to_string(), signed.txid.value());
    for txin in &tx.input {
        let items = if txin.previous_output == wallet.coin(1) { 1 } else { 2 };
        assert_eq!(txin.witness.len(), items, "{}", txin.previous_output);
    }

    let error = wallet
        .harness
        .adapter
        .build_tx(&wallet.send(&[NATIVE, TAPROOT], PAYMENT_SAT))
        .await
        .expect_err("the claimed coins cannot pay twice");
    assert!(matches!(error, ChainError::InsufficientFunds { .. }), "{error}");
}

/// A send-max quotes and drains every coin of every source.
#[tokio::test]
async fn a_send_max_spends_the_total_of_every_source() {
    let wallet = wallet().await.unwrap();
    let request = TxRequestBuilder::new(
        NATIVE,
        wallet.theirs.address(),
        AssetId::Native(BITCOIN.chain),
        sats(0),
    )
    .sources(&[NATIVE, TAPROOT])
    .send_max()
    .fee(FeeSelection::Rate { rate: RATE.to_owned() })
    .build();
    let quote = wallet.harness.adapter.quote_fee(&request).await.unwrap();
    let total = sats(60_000 + 70_000 + 50_000);
    assert_eq!(quote.max_amount.and_then(|amount| amount.checked_add(quote.fee)), Some(total));

    let unsigned = wallet.harness.adapter.build_tx(&request).await.unwrap();
    assert_eq!(unsigned.preview.amount, quote.max_amount.unwrap());
    let mut expected = vec![wallet.coin(0), wallet.coin(1), wallet.coin(2)];
    expected.sort();
    assert_eq!(spent(&unsigned).unwrap(), expected);
}

/// The first source receives the change even when the selection needed none of
/// its coins, and a source with nothing to give and no change is left out of what
/// the transaction names.
#[tokio::test]
async fn the_preview_names_only_the_sources_the_transaction_moves() {
    let wallet = wallet().await.unwrap();
    let change_only = wallet
        .harness
        .adapter
        .build_tx(&wallet.send(&[NATIVE, LEGACY], PAYMENT_SAT))
        .await
        .unwrap();
    assert_eq!(spent(&change_only).unwrap(), [wallet.coin(3)]);
    assert_eq!(
        change_only.preview.from,
        [wallet.native.address().to_owned(), wallet.legacy.address().to_owned()]
    );
    let keys = wallet.keys_for(&change_only).unwrap();
    wallet.harness.adapter.sign_tx(&change_only, &keys).unwrap();

    let request = TxRequestBuilder::new(
        EMPTY,
        wallet.theirs.address(),
        AssetId::Native(BITCOIN.chain),
        sats(0),
    )
    .sources(&[EMPTY, TAPROOT])
    .send_max()
    .fee(FeeSelection::Rate { rate: RATE.to_owned() })
    .build();
    let drained = wallet.harness.adapter.build_tx(&request).await.unwrap();
    assert_eq!(drained.preview.from, [wallet.taproot.address().to_owned()]);
}

#[tokio::test]
async fn a_send_with_no_source_or_an_unwatched_one_is_refused() {
    let wallet = wallet().await.unwrap();
    for sources in [&[][..], &[NATIVE, 99][..]] {
        let error = wallet
            .harness
            .adapter
            .build_tx(&wallet.send(sources, PAYMENT_SAT))
            .await
            .expect_err("no usable source");
        assert!(matches!(error, ChainError::InvalidInput(..)), "{sources:?}: {error}");
    }
}

/// The replacement keeps both original inputs, and when the change cannot cover
/// the higher fee it adds the taproot address's other coin, never the legacy
/// address's, which the original did not spend from.
#[tokio::test]
async fn a_replacement_keeps_every_input_and_adds_coins_only_from_its_sources() {
    let wallet = wallet().await.unwrap();
    let unsigned = wallet
        .harness
        .adapter
        .build_tx(&wallet.send(&[NATIVE, TAPROOT], PAYMENT_SAT))
        .await
        .unwrap();
    let keys = wallet.keys_for(&unsigned).unwrap();
    let signed = wallet.harness.adapter.sign_tx(&unsigned, &keys).unwrap();
    let original = decode(signed.raw.as_bytes()).unwrap();
    wallet.ledger.clone().known(&original).serve(&wallet.harness, &wallet.keys());

    let bump = wallet.harness.adapter.fee_bump().expect("bitcoin replaces");
    let rate = FeeRate::from_sat_per_kwu(15_000);
    let replacement = bump.build_bump(&signed.txid, &at(rate)).await.unwrap();
    let mut expected = vec![wallet.coin(0), wallet.coin(1), wallet.coin(2)];
    expected.sort();
    assert_eq!(spent(&replacement).unwrap(), expected);
    assert_eq!(
        replacement.preview.from,
        [wallet.native.address().to_owned(), wallet.taproot.address().to_owned()]
    );
    assert_eq!(replacement.preview.amount, sats(PAYMENT_SAT), "the payment is untouched");
    let keys = wallet.keys_for(&replacement).unwrap();
    wallet.harness.adapter.sign_tx(&replacement, &keys).unwrap();
}
