//! The preview is rendered from the same [`Plan`] the transaction was built with,
//! so it describes the bytes that will be signed.
//!
//! Every figure that could be wrong says so, such as a price the node didn't
//! report or a surcharge that can rise before execution.

use jelly_core::{Amount, AssetId, TxDetail, TxPreview, TxWarning, TxWarningKind};

use crate::config::CHAIN;
use crate::fee::{BandwidthSource, EnergyCost, TxCost, trx_text};
use crate::params::ChainParams;
use crate::payload::Facts;
use crate::raw::Header;
use crate::send::Plan;

pub(crate) fn of(plan: &Plan, facts: &Facts) -> TxPreview {
    TxPreview {
        chain: CHAIN,
        asset: facts.asset,
        from: vec![facts.owner.clone()],
        to: facts.recipient.clone(),
        destination_tag: None,
        amount: facts.amount,
        fee: Amount::from(plan.cost.burn_sun),
        rent: None,
        details: details(plan, facts),
        warnings: warnings(plan, facts),
    }
}

fn details(plan: &Plan, facts: &Facts) -> Vec<TxDetail> {
    let cost = &plan.cost;
    let mut details = vec![
        TxDetail::new("Contract", format!("{} transfer", facts.asset.symbol())),
        TxDetail::new("Transaction id", plan.payload.txid_hex()),
        TxDetail::new(
            "Reference block",
            format!(
                "{} {}",
                hex::encode(facts.header.ref_block_bytes),
                hex::encode(facts.header.ref_block_hash)
            ),
        ),
        TxDetail::new("Expires", expiry(&facts.header)),
        TxDetail::new("Bandwidth", format!("{} points", cost.bandwidth.points)),
        TxDetail::new("Bandwidth charge", bandwidth_charge(cost, &plan.params)),
    ];
    if let AssetId::Token(token) = facts.asset {
        details.push(TxDetail::new("Token contract", token.contract()));
        details.push(TxDetail::new("Call data bytes", facts.calldata.len().to_string()));
    }
    if let Some(fee_limit_sun) =
        u64::try_from(facts.header.fee_limit_sun).ok().filter(|sun| *sun > 0)
    {
        details.push(TxDetail::new("Fee limit", fee_limit(fee_limit_sun, &plan.params)));
    }
    if let Some(energy) = cost.energy {
        details.push(TxDetail::new(
            "Energy",
            format!("{} needed, {} available", energy.needed, energy.available),
        ));
        details.push(TxDetail::new("Energy charge", energy_charge(&energy, &plan.params)));
    }
    details.push(TxDetail::new("Estimated burn", trx_text(cost.burn_sun)));
    details.push(TxDetail::new("Raw data bytes", plan.payload.raw_data().len().to_string()));
    details.push(TxDetail::new("Prices", prices(&plan.params)));
    details
}

fn warnings(plan: &Plan, facts: &Facts) -> Vec<TxWarning> {
    let cost = &plan.cost;
    let params = &plan.params;
    let mut warnings = Vec::new();
    if cost.is_activation {
        warnings.push(TxWarning::new(
            TxWarningKind::AccountActivation,
            format!(
                "Activating {} on TRON costs an extra {}.",
                facts.recipient,
                trx_text(cost.activation_fee_sun)
            ),
        ));
    }
    if cost.bandwidth.source == BandwidthSource::Burned && !cost.is_activation {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "Neither bandwidth pool covers all {} points. This burns {} instead.",
                cost.bandwidth.points,
                trx_text(cost.bandwidth.burn_sun)
            ),
        ));
    }
    if let Some(energy) = cost.energy {
        if energy.shortfall > 0 {
            warnings.push(TxWarning::new(
                TxWarningKind::Other,
                format!(
                    "This needs {} more energy and will burn {} to cover it.",
                    energy.shortfall,
                    trx_text(energy.burn_sun)
                ),
            ));
        }
        if energy.penalty > 0 {
            warnings.push(TxWarning::new(
                TxWarningKind::Estimated,
                format!(
                    "The contract surcharge accounts for {} of {} energy. It can increase before \
                     confirmation, raising the cost.",
                    energy.penalty, energy.needed
                ),
            ));
        }
    }
    if matches!(facts.asset, AssetId::Token(_)) {
        // A token send never reads the recipient's account, so this note always
        // shows. A TRC-20 transfer never activates an address.
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "Sending {} does not activate {} on TRON. The recipient will also need TRX to move \
                 the tokens.",
                facts.asset.symbol(),
                facts.recipient
            ),
        ));
    }
    if !params.is_from_chain {
        warnings.push(TxWarning::new(
            TxWarningKind::Estimated,
            "The node did not provide fee prices. This estimate uses Jelly's defaults.",
        ));
    }
    warnings
}

/// How long the transaction stays valid, in seconds below a minute and in whole
/// minutes from there.
fn expiry(header: &Header) -> String {
    let window_secs = header.window_secs();
    let (count, unit) = if window_secs < 60 {
        (window_secs, "second")
    } else {
        (window_secs.saturating_div(60), "minute")
    };
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {unit}{plural} after building")
}

fn bandwidth_charge(cost: &TxCost, params: &ChainParams) -> String {
    match cost.bandwidth.source {
        BandwidthSource::Staked => {
            String::from("Covered by your staked bandwidth. No TRX is burned.")
        }
        BandwidthSource::Free => {
            String::from("Covered by your free daily bandwidth. No TRX is burned.")
        }
        BandwidthSource::Burned if cost.is_activation => format!(
            "There is not enough staked bandwidth, so a flat {} account-creation fee is burned.",
            trx_text(cost.bandwidth.burn_sun)
        ),
        BandwidthSource::Burned => format!(
            "Neither pool covers all {} points on its own, so the whole cost is burned at {} sun \
             each, which is {}.",
            cost.bandwidth.points,
            params.bandwidth_price_sun,
            trx_text(cost.bandwidth.burn_sun)
        ),
    }
}

fn energy_charge(energy: &EnergyCost, params: &ChainParams) -> String {
    if energy.shortfall == 0 {
        return String::from("Covered by your staked energy. No TRX is burned.");
    }
    format!(
        "The call is {} energy short, which burns {} sun each, {} in all.",
        energy.shortfall,
        params.energy_price_sun,
        trx_text(energy.burn_sun)
    )
}

/// What the limit is and what it caps, at the current energy price.
fn fee_limit(fee_limit_sun: u64, params: &ChainParams) -> String {
    let ceiling = fee_limit_sun.checked_div(params.energy_price_sun.max(1)).unwrap_or_default();
    format!(
        "The fee limit is {}. It caps this call at {ceiling} energy at the current price and is \
         a ceiling, not a charge.",
        trx_text(fee_limit_sun)
    )
}

/// One line naming both prices and where they came from.
fn prices(params: &ChainParams) -> String {
    let source = if params.is_from_chain { "reported by the node" } else { "Jelly's defaults" };
    format!(
        "Bandwidth {} sun per point, energy {} sun per unit ({source})",
        params.bandwidth_price_sun, params.energy_price_sun
    )
}
