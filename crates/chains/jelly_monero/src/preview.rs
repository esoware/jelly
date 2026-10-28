use jelly_core::{Amount, TxDetail, TxPreview, TxWarning, TxWarningKind};

use crate::config::MoneroConfig;
use crate::fee::PAYMENT_AND_CHANGE;
use crate::params::{ASSET, CHAIN, FEE_RATE_UNIT, RING_LEN, piconero};
use crate::send::{Built, Plan};

/// A scan gap wider than this warns that the view of the chain is stale.
const STALE_SCAN_WARN_BLOCKS: u64 = 720;

/// What a person is shown before they sign.
///
/// Every figure comes off the assembled transaction, not an estimate, so the fee
/// is the one it commits to and the change is what the inputs leave.
/// `ring_height` is the block its rings were chosen against.
pub(crate) fn of(config: &MoneroConfig, plan: &Plan, built: &Built, ring_height: u64) -> TxPreview {
    TxPreview {
        chain: CHAIN,
        asset: ASSET,
        from: vec![plan.from.clone()],
        to: plan.recipient.canonical().to_owned(),
        destination_tag: None,
        amount: piconero(built.sent_piconero),
        fee: piconero(built.fee_piconero),
        rent: None,
        details: details(plan, built, ring_height),
        warnings: warnings(config, plan, built),
    }
}

fn details(plan: &Plan, built: &Built, ring_height: u64) -> Vec<TxDetail> {
    let mut details = vec![
        TxDetail::new("Ring size", RING_LEN.to_string()),
        TxDetail::new("Inputs", built.input_count.to_string()),
        TxDetail::new("Outputs", format!("{PAYMENT_AND_CHANGE} (payment and change)")),
        TxDetail::new("Change", CHAIN.format_amount(piconero(built.change_piconero))),
        TxDetail::new(
            "Change goes to",
            format!("Account {} primary address", plan.subaddress.major),
        ),
        TxDetail::new(
            "Total debited",
            CHAIN.format_amount(piconero(
                built.input_sum_piconero.saturating_sub(built.change_piconero),
            )),
        ),
        TxDetail::new("Fee rate", format!("{} {FEE_RATE_UNIT}", plan.rate.per_weight())),
        TxDetail::new("Decoys chosen at height", ring_height.to_string()),
        TxDetail::new("Address type", plan.recipient.kind().label()),
    ];
    if let Some(priority) = plan.priority {
        details.push(TxDetail::new("Priority", priority.label()));
    }
    details
}

fn warnings(config: &MoneroConfig, plan: &Plan, built: &Built) -> Vec<TxWarning> {
    let mut warnings = vec![TxWarning::new(
        TxWarningKind::RemoteDecoys,
        "A remote node supplies the decoy data. A malicious node can influence decoy selection and weaken your privacy.",
    )];
    let locked = plan.balance.locked();
    if locked > Amount::ZERO {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!("{} is still locked and cannot be spent yet.", CHAIN.format_amount(locked)),
        ));
    }
    let reserved = plan.balance.reserved();
    if reserved > Amount::ZERO {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!("Pending sends have reserved {}.", CHAIN.format_amount(reserved)),
        ));
    }
    if plan.scan_gap > STALE_SCAN_WARN_BLOCKS {
        warnings.push(TxWarning::new(
            TxWarningKind::Estimated,
            format!(
                "This balance may be outdated. The wallet has not scanned the latest {} blocks.",
                plan.scan_gap
            ),
        ));
    }
    if plan.recipient.payment_id().is_some() {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            "This integrated address includes a payment ID. Integrated addresses are deprecated. Use a subaddress when possible.",
        ));
    }
    if built.change_piconero == 0 && plan.target_piconero.is_some() {
        warnings.push(TxWarning::new(
            TxWarningKind::DroppedChange,
            "This payment uses the selected coins exactly and leaves no change.",
        ));
    }
    if built.input_count >= config.inputs() {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "This uses the maximum of {} inputs. Split larger payments into separate sends.",
                config.inputs()
            ),
        ));
    }
    warnings
}
