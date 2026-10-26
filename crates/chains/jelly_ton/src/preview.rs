use jelly_core::{Amount, TxDetail, TxPreview, TxWarning, TxWarningKind};

use crate::config::CHAIN;
use crate::error::TonError;
use crate::payload::{Payment, Transfer};

pub(crate) fn of(payment: &Payment) -> Result<TxPreview, TonError> {
    let mut details = vec![
        TxDetail::new("Network", "TON mainnet"),
        TxDetail::new("Wallet contract", payment.version.name()),
        TxDetail::new("Sequence", payment.seqno.to_string()),
    ];
    let mut warnings = vec![TxWarning::new(
        TxWarningKind::Estimated,
        "The network fee is an estimate. TON charges the actual execution and storage fees.",
    )];
    if payment.is_deploy {
        warnings.push(TxWarning::new(
            TxWarningKind::AccountActivation,
            "This payment also deploys your wallet contract. Its deployment fee is included in the estimate.",
        ));
    }
    match payment.transfer {
        Transfer::Native { is_bounceable, .. } => {
            details.push(TxDetail::new(
                "Bounce protection",
                if is_bounceable { "Enabled" } else { "Disabled" },
            ));
            if payment.is_max {
                warnings.push(TxWarning::new(
                    TxWarningKind::Estimated,
                    "The recipient receives your remaining balance after the actual network fee. The final amount may differ from this estimate.",
                ));
            }
        }
        Transfer::Jetton { attached_nanogram, .. } => {
            let attached = CHAIN.format_amount(Amount::from(attached_nanogram));
            warnings.push(TxWarning::new(
                TxWarningKind::Estimated,
                format!(
                    "The fee includes {attached} for the token wallets. What they do not spend comes back to you."
                ),
            ));
            details.push(TxDetail::new("Gas for the token wallets", attached));
        }
    }
    let comment = payment.comment();
    if let Some(comment) = comment {
        details.push(TxDetail::new("Comment", comment));
    }
    if comment.is_none_or(str::is_empty) {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            "No comment is included. Some exchanges require a comment to credit your deposit.",
        ));
    }
    Ok(TxPreview {
        chain: CHAIN,
        asset: payment.asset(),
        from: vec![payment.from.clone()],
        to: payment.to.clone(),
        destination_tag: None,
        amount: payment.amount(),
        fee: Amount::from(payment.cost_nanogram()?),
        details,
        warnings,
    })
}
