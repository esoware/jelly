use jelly_core::{Amount, AssetId, TxDetail, TxPreview, TxWarning, TxWarningKind};

use zcash_transparent::address::Script;

use crate::address::script;
use crate::config::CHAIN;
use crate::fee::conventional_fee;
use crate::payload::Payment;

pub(crate) fn zec(zat: u64) -> String {
    CHAIN.format_amount(Amount::from(zat))
}

pub(crate) fn of(payment: &Payment) -> TxPreview {
    let mut warnings = vec![TxWarning::new(
        TxWarningKind::Other,
        "This is a transparent payment. The addresses and amount are public.",
    )];
    if payment.dropped_change_zat > 0 {
        warnings.push(TxWarning::new(
            TxWarningKind::DroppedChange,
            "Change below the minimum output amount is added to the network fee.",
        ));
    }
    let mut details = vec![
        TxDetail::new("Payment type", "Transparent"),
        TxDetail::new("Inputs", payment.inputs.len().to_string()),
    ];
    if let Some(conventional_zat) = conventional_zat(payment) {
        details.push(TxDetail::new("ZIP 317 fee", zec(conventional_zat)));
    }
    details.push(TxDetail::new("Expires after block", payment.expiry_height.to_string()));
    details.push(TxDetail::new("Change", zec(payment.change_zat)));
    TxPreview {
        chain: CHAIN,
        asset: AssetId::Native(CHAIN),
        from: payment.sources.iter().map(|source| source.address.clone()).collect(),
        to: payment.to.clone(),
        destination_tag: None,
        amount: Amount::from(payment.amount_zat),
        fee: Amount::from(payment.fee_zat),
        details,
        warnings,
    }
}

/// The ZIP 317 fee of the outputs the payment actually has, which is what a custom fee
/// is measured against.
fn conventional_zat(payment: &Payment) -> Option<u64> {
    let recipient = script(&payment.to).ok()?;
    let change = if payment.change_zat > 0 {
        Some(script(payment.change_address().ok()?).ok()?)
    } else {
        None
    };
    let outputs: Vec<&Script> = [Some(&recipient), change.as_ref()].into_iter().flatten().collect();
    conventional_fee(payment.target_height, &payment.inputs, &outputs).ok()
}
