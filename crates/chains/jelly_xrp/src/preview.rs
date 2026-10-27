use jelly_core::{Amount, AssetId, TxDetail, TxPreview, TxWarning, TxWarningKind};

use crate::config::CHAIN;
use crate::payload::{Payment, SendKind};

pub(crate) fn xrp(drops: u64) -> String {
    CHAIN.format_amount(Amount::from(drops))
}

pub(crate) fn of(payment: &Payment) -> TxPreview {
    let mut details = Vec::new();
    let mut warnings = Vec::new();
    match payment.kind {
        SendKind::Payment => {
            details.push(TxDetail::new("Sequence", payment.sequence.to_string()));
            details.push(TxDetail::new("Valid through ledger", payment.last_ledger.to_string()));
            details.push(TxDetail::new("Account reserve", xrp(payment.reserve_drops)));
            warnings.push(TxWarning::new(
                TxWarningKind::Other,
                "The account reserve stays in your wallet and cannot be sent in a payment.",
            ));
            if payment.is_activation {
                warnings.push(TxWarning::new(
                    TxWarningKind::AccountActivation,
                    format!(
                        "This payment activates the recipient's account. {} of it becomes the account's reserve.",
                        xrp(payment.base_reserve_drops)
                    ),
                ));
            }
        }
        SendKind::AccountDelete => {
            details.push(TxDetail::new("Transaction", "Account deletion"));
            details.push(TxDetail::new("Sequence", payment.sequence.to_string()));
            details.push(TxDetail::new("Valid through ledger", payment.last_ledger.to_string()));
            details
                .push(TxDetail::new("Reserve to receive again", xrp(payment.base_reserve_drops)));
            warnings.push(TxWarning::new(
                TxWarningKind::AccountDeletion,
                format!(
                    "This removes your XRP account from the ledger and sends everything it holds, less the deletion fee, to the recipient. To receive XRP at this address again, it must first be sent at least {} to recreate the account.",
                    xrp(payment.base_reserve_drops)
                ),
            ));
        }
    }
    match payment.destination_tag {
        Some(tag) => details.push(TxDetail::new("Destination tag", tag.to_string())),
        None => warnings.push(TxWarning::new(
            TxWarningKind::Other,
            "No destination tag is included. Exchanges may require one even when the network does not. Confirm the recipient's deposit instructions.",
        )),
    }
    TxPreview {
        chain: CHAIN,
        asset: AssetId::Native(CHAIN),
        from: vec![payment.from.clone()],
        to: payment.to.clone(),
        destination_tag: payment.destination_tag,
        amount: Amount::from(payment.amount_drops),
        fee: Amount::from(payment.fee_drops),
        details,
        warnings,
    }
}
