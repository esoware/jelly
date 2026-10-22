//! Speeding up a Litecoin send with a child that spends its change.
//!
//! Litecoin Core relays no replacement by default, so the bump is a second
//! transaction. These tests prove what it spends, who it pays, and that its fee lifts
//! the whole unconfirmed package, read off the chain after a restart as well.

use bitcoin::{OutPoint, Transaction};
use jelly_core::{
    Amount, ChainAdapter as _, ChainError, FeeRate, FeeSelection, ScriptKind, SignedTx, TxId,
    TxWarningKind, UnsignedTx,
};
use jelly_utxo::test_util::tx_spending;

use crate::common::{
    CHANGE_VOUT, Fallible, Harness, LITECOIN, Ledger, MINED_AT, Setup, Wallet, at, decode, drain,
    payload, sats, synced,
};

const FUNDING_SAT: u64 = 500_000;
/// Where a build puts change, as an index into the outputs.
const CHANGE_INDEX: usize = 1;
const PAYMENT_SAT: u64 = 100_000;
const RATE: &str = "4";
/// A hundred litoshi per virtual byte, so a package's fee at it is its size times a
/// hundred.
const HIGHER: FeeRate = FeeRate::from_sat_per_kwu(25_000);
const HIGHER_PER_VB: u64 = 100;

/// A send the last pass saw in the mempool, with what it spent and its change.
struct Sent {
    harness: Harness,
    wallet: Wallet,
    signed: SignedTx,
    tx: Transaction,
}

/// What a transaction pays and how many virtual bytes it is.
fn priced(tx: &Transaction, spent: u64) -> (u64, u64) {
    let paid: u64 = tx.output.iter().map(|txout| txout.value.to_sat()).sum();
    (spent.saturating_sub(paid), tx.weight().to_vbytes_ceil())
}

/// A Litecoin wallet that sent `amount`, or everything for `None`, and whose next
/// pass saw the send in the mempool.
async fn sent(amount: Option<u64>) -> Fallible<Sent> {
    let (harness, wallet) =
        synced(Setup::on(&LITECOIN), ScriptKind::P2wpkh, &[FUNDING_SAT]).await?;
    let request = match amount {
        Some(amount) => harness.request(wallet.theirs.address(), amount, RATE),
        None => drain(&LITECOIN, wallet.theirs.address(), at(FeeRate::from_sat_per_kwu(1_000))),
    };
    let unsigned = harness.adapter.build_tx(&request).await?;
    let signed = harness.adapter.sign_tx(&unsigned, &[wallet.ours.material()])?;
    let tx = decode(signed.raw.as_bytes())?;
    let mut ledger = Ledger::new().known(&wallet.funding).confirmed(&wallet.payment, MINED_AT);
    ledger = ledger.mempool(&tx);
    if let Some(change) = tx.output.get(usize::try_from(CHANGE_VOUT)?) {
        ledger = ledger.unconfirmed(&tx, CHANGE_VOUT, change.value.to_sat());
    }
    ledger.serve(&harness);
    harness.sync().await?;
    Ok(Sent { harness, wallet, signed, tx })
}

impl Sent {
    async fn bump(&self, txid: &TxId, fee: &FeeSelection) -> Result<UnsignedTx, ChainError> {
        let bump = self
            .harness
            .adapter
            .fee_bump()
            .ok_or_else(|| ChainError::unsupported(String::new()))?;
        bump.build_bump(txid, fee).await
    }
}

/// What a child of `child_vsize` virtual bytes pays at [`HIGHER`] for a package of
/// `members`, each its fee and virtual bytes: the rate over everything, less what the
/// members already paid.
fn package_fee(members: &[(u64, u64)], child_vsize: u64) -> Amount {
    let vsize = members.iter().map(|(_, vsize)| vsize).sum::<u64>().saturating_add(child_vsize);
    let paid: u64 = members.iter().map(|(fee, _)| fee).sum();
    sats(vsize.saturating_mul(HIGHER_PER_VB).saturating_sub(paid))
}

/// The virtual size the child was priced at, as its preview states it.
fn priced_vsize(child: &UnsignedTx) -> u64 {
    child
        .preview
        .details
        .iter()
        .find(|row| row.label == "Virtual size")
        .and_then(|row| row.value.trim_end_matches(" vB").parse().ok())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_litecoin_send_is_sped_up_by_a_child_spending_its_change() {
    let sent = sent(Some(PAYMENT_SAT)).await.unwrap();
    let quote = sent
        .harness
        .adapter
        .fee_bump()
        .unwrap()
        .quote_bump(&sent.signed.txid, &at(HIGHER))
        .await
        .unwrap();
    let child = sent.bump(&sent.signed.txid, &at(HIGHER)).await.unwrap();
    let decoded = payload(&child).unwrap();

    let change = OutPoint { txid: sent.tx.compute_txid(), vout: CHANGE_VOUT };
    assert_eq!(decoded.tx.input.first().map(|txin| txin.previous_output), Some(change));
    assert_eq!(decoded.tx.input.len(), 1, "the change pays for itself");
    assert_eq!(decoded.tx.output.len(), 1);
    assert_eq!(decoded.to, sent.wallet.ours.address(), "the child pays back the first source");
    assert_eq!(decoded.pays_for, Some(sent.tx.compute_txid()));
    assert_eq!(decoded.strategy, "child_pays_for_parent");

    let parent = priced(&sent.tx, FUNDING_SAT);
    let expected = package_fee(&[parent], priced_vsize(&child));
    assert_eq!(decoded.fee, expected, "the package of parent and child reaches the rate");
    assert_eq!(quote.fee, decoded.fee, "the quote prices the child that gets built");

    assert!(child.preview.amount.is_zero(), "nothing leaves the wallet but the fee");
    assert_eq!(child.preview.fee, decoded.fee);
    assert!(
        child.preview.warnings.iter().any(|warning| warning.kind == TxWarningKind::ChildPays
            && warning.message.contains("second transaction")),
        "{:?}",
        child.preview.warnings
    );
    assert!(
        child
            .preview
            .details
            .iter()
            .any(|row| row.label == "Pays for" && row.value == sent.tx.compute_txid().to_string())
    );
    sent.harness.adapter.sign_tx(&child, &[sent.wallet.ours.material()]).unwrap();
}

#[tokio::test]
async fn a_send_with_no_change_cannot_be_sped_up() {
    let sent = sent(None).await.unwrap();
    assert_eq!(sent.tx.output.len(), 1, "a send-max pays out everything");
    let error = sent.bump(&sent.signed.txid, &at(HIGHER)).await.unwrap_err();
    assert!(
        matches!(&error, ChainError::InvalidInput(text, _) if text.contains("has no change output")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_child_at_a_level_pays_at_least_just_above_the_package() {
    let sent = sent(Some(PAYMENT_SAT)).await.unwrap();
    let quote = sent
        .harness
        .adapter
        .fee_bump()
        .unwrap()
        .quote_bump(&sent.signed.txid, &FeeSelection::NORMAL)
        .await
        .unwrap();
    let Some(FeeSelection::Rate { rate: sat_per_vb }) = quote.custom else {
        panic!("a utxo rate");
    };
    let chosen = FeeRate::parse_sat_per_vb(&sat_per_vb).unwrap();
    let parent = priced(&sent.tx, FUNDING_SAT);
    let parent_rate = parent.0.saturating_mul(1_000).div_euclid(parent.1);
    assert!(chosen.sat_per_kwu().saturating_mul(4) > parent_rate, "{chosen:?}");
    let refused =
        sent.bump(&sent.signed.txid, &at(FeeRate::from_sat_per_kwu(500))).await.unwrap_err();
    assert!(matches!(refused, ChainError::InvalidInput(..)), "{refused:?}");
}

/// A send that spends the change of another still in the mempool waits on it, so the
/// child pays for both.
#[tokio::test]
async fn a_child_pays_for_every_unconfirmed_ancestor_the_wallet_sees() {
    let sent = sent(Some(PAYMENT_SAT)).await.unwrap();
    let first_change = sent.tx.output[CHANGE_INDEX].value.to_sat();
    let unsigned = sent
        .harness
        .adapter
        .build_tx(&sent.harness.request(sent.wallet.theirs.address(), PAYMENT_SAT, RATE))
        .await
        .unwrap();
    let second = sent.harness.adapter.sign_tx(&unsigned, &[sent.wallet.ours.material()]).unwrap();
    let second_tx = decode(second.raw.as_bytes()).unwrap();
    assert_eq!(
        second_tx.input.first().map(|txin| txin.previous_output.txid),
        Some(sent.tx.compute_txid()),
        "the second send spends the first one's change"
    );
    let second_change = second_tx.output[CHANGE_INDEX].value.to_sat();
    Ledger::new()
        .known(&sent.wallet.funding)
        .confirmed(&sent.wallet.payment, MINED_AT)
        .mempool(&sent.tx)
        .mempool(&second_tx)
        .unconfirmed(&second_tx, CHANGE_VOUT, second_change)
        .serve(&sent.harness);
    sent.harness.sync().await.unwrap();

    let child = sent.bump(&second.txid, &at(HIGHER)).await.unwrap();
    let decoded = payload(&child).unwrap();
    assert_eq!(decoded.pays_for, Some(second_tx.compute_txid()));
    let child_vsize = priced_vsize(&child);
    let members = [priced(&second_tx, first_change), priced(&sent.tx, FUNDING_SAT)];
    assert_eq!(decoded.fee, package_fee(&members, child_vsize));
    let alone = package_fee(&members[..1], child_vsize);
    assert!(decoded.fee > alone, "the ancestor is lifted too");
}

#[tokio::test]
async fn a_change_a_child_already_spends_names_the_transaction_to_speed_up_instead() {
    let sent = sent(Some(PAYMENT_SAT)).await.unwrap();
    let change = &sent.tx.output[CHANGE_INDEX];
    let child = tx_spending(
        &sent.tx,
        CHANGE_VOUT,
        &[(sent.wallet.ours.script_pubkey(), change.value.to_sat().saturating_sub(5_000))],
    );
    Ledger::new()
        .known(&sent.wallet.funding)
        .confirmed(&sent.wallet.payment, MINED_AT)
        .mempool(&sent.tx)
        .mempool(&child)
        .unconfirmed(&child, 0, change.value.to_sat().saturating_sub(5_000))
        .serve(&sent.harness);
    sent.harness.sync().await.unwrap();
    let error = sent.bump(&sent.signed.txid, &at(HIGHER)).await.unwrap_err();
    assert!(
        matches!(&error, ChainError::InvalidInput(text, _) if text.contains("already spent")),
        "{error:?}"
    );
}
