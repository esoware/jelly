//! The preview shows two fees on purpose.
//!
//! Its fee is the expected cost, what the transaction pays if it lands in the next block, so
//! it matches the quote the user saw. The worst case, which the node reserves and a send-max
//! subtracts, is a detail row.

use alloy::hex;
use jelly_core::{Amount, TxDetail, TxPreview, TxWarning, TxWarningKind};

use crate::error::EvmError;
use crate::fee::FeeMarket;
use crate::params::EvmParams;
use crate::payload::EvmPayload;
use crate::send::Plan;
use crate::token::TokenTable;

/// Wei per gwei, as a decimal exponent.
const GWEI_DECIMALS: u8 = 9;

pub(crate) fn of(
    plan: &Plan,
    built: &EvmPayload,
    tokens: &TokenTable,
    worst_case_fee_wei: u128,
) -> Result<TxPreview, EvmError> {
    let params = plan.params;
    let decimals = plan.asset.decimals();
    let asset_label = match plan.token {
        Some(token) => format!("{} ({})", token.id().coin().symbol(), params.token_standard),
        None => params.symbol().to_owned(),
    };
    let amount_symbol = plan.asset.coin().symbol();
    let calldata = built.input();
    let target = built.call_target().unwrap_or(plan.to);

    let mut details = vec![
        TxDetail::new("Chain id", params.chain_id.to_string()),
        TxDetail::new("Asset", asset_label),
        TxDetail::new("Amount", format!("{} {amount_symbol}", plan.amount.format(decimals))),
        TxDetail::new("Recipient", plan.to.to_string()),
        TxDetail::new("Transaction to", target.to_string()),
        TxDetail::new("Value in wei", built.value().to_string()),
        TxDetail::new("Nonce", plan.nonce.to_string()),
        TxDetail::new("Gas limit", plan.gas.limit().to_string()),
        TxDetail::new("Gas limit source", plan.gas.source().label()),
        TxDetail::new(
            "Max fee per gas",
            format!("{} ({} wei)", gwei(plan.fee.max_fee_per_gas()), plan.fee.max_fee_per_gas()),
        ),
        TxDetail::new(
            "Max priority fee per gas",
            format!(
                "{} ({} wei)",
                gwei(plan.fee.max_priority_fee_per_gas()),
                plan.fee.max_priority_fee_per_gas()
            ),
        ),
        TxDetail::new(
            "Max total fee",
            format!("{} ({worst_case_fee_wei} wei)", coins(worst_case_fee_wei, params)),
        ),
        TxDetail::new("Transaction type", "EIP-1559 (type 2)"),
        TxDetail::new(
            "Calldata",
            if calldata.is_empty() {
                String::from("None")
            } else {
                format!("0x{}", hex::encode(calldata))
            },
        ),
        TxDetail::new("Calldata bytes", calldata.len().to_string()),
    ];

    if let Some(token) = plan.token {
        details.push(TxDetail::new("Token contract", token.address().to_string()));
        details.push(TxDetail::new("Token decimals", token.id().decimals().to_string()));
    }
    if params.has_l1_data_fee {
        details.push(TxDetail::new(
            "L1 data fee",
            format!("{} ({} wei)", coins(plan.data_fee_wei, params), plan.data_fee_wei),
        ));
    }
    let expected_fee_wei = plan.expected_fee_wei()?;
    if let Some(market) = plan.market {
        details.push(TxDetail::new("Base fee", gwei(market.base_fee_per_gas())));
        details.push(TxDetail::new(
            "Estimated fee",
            format!("{} ({expected_fee_wei} wei)", coins(expected_fee_wei, params)),
        ));
    }

    let native_cost_wei = plan.native_cost_wei()?;
    let left_wei = plan.native_balance_wei.saturating_sub(native_cost_wei);
    details.push(TxDetail::new(
        "Maximum total cost",
        if plan.token.is_some() {
            format!(
                "{} {amount_symbol} plus up to {} in gas",
                plan.amount.format(decimals),
                coins(worst_case_fee_wei, params)
            )
        } else {
            format!("Up to {}", coins(native_cost_wei, params))
        },
    ));
    details.push(TxDetail::new("Native balance after", coins(left_wei, params)));

    Ok(TxPreview {
        chain: params.chain,
        asset: plan.asset,
        from: vec![plan.from.to_string()],
        to: plan.to.to_string(),
        destination_tag: None,
        amount: plan.amount,
        fee: Amount::from_base_units(expected_fee_wei),
        rent: None,
        details,
        warnings: warnings(plan, tokens, worst_case_fee_wei, left_wei),
    })
}

fn warnings(
    plan: &Plan,
    tokens: &TokenTable,
    worst_case_fee_wei: u128,
    left_wei: u128,
) -> Vec<TxWarning> {
    let params = plan.params;
    let mut warnings = Vec::new();

    if let Some(floor) = plan.replaces {
        let replacement = if plan.is_cancel {
            format!(
                "This cancels the transaction at nonce {} by sending nothing to yourself in its place. Only one can confirm. If the original confirms first, this one never runs and costs nothing.",
                plan.nonce
            )
        } else {
            format!("This replaces the transaction at nonce {}. Only one can confirm.", plan.nonce)
        };
        warnings.push(TxWarning::new(
            TxWarningKind::Replacement,
            format!(
                "{replacement} The maximum fee is {} per gas. The required minimum is {}.",
                gwei(plan.fee.max_fee_per_gas()),
                gwei(floor.max_fee_per_gas())
            ),
        ));
    }
    if let Some(base_fee_per_gas) = plan.market.map(FeeMarket::base_fee_per_gas)
        && plan.fee.max_fee_per_gas() < base_fee_per_gas
    {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "The maximum fee of {} per gas is below the current base fee of {}. The transaction waits until the base fee drops.",
                gwei(plan.fee.max_fee_per_gas()),
                gwei(base_fee_per_gas)
            ),
        ));
    }
    if plan.calldata.is_some() {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            "Custom calldata overrides the transfer shown above. Check the data before signing.",
        ));
    }
    if plan.amount.is_zero() && !plan.is_cancel {
        warnings.push(TxWarning::new(TxWarningKind::Other, "The transfer amount is zero."));
    }
    if plan.to == plan.from && !plan.is_cancel {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            "You are sending to the same address. You will still pay gas.",
        ));
    }
    if tokens.at(plan.to).is_some() {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            "This recipient is a token contract. Funds sent here may be unrecoverable.",
        ));
    }
    if plan.is_nonce_pinned && plan.replaces.is_none() {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "You set nonce {} manually. A used nonce cannot confirm. A higher nonce must wait for earlier transactions.",
                plan.nonce
            ),
        ));
    }
    if plan.should_send_max && plan.token.is_none() {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "At the maximum gas fee, this leaves {} and costs {} in fees.",
                coins(left_wei, params),
                coins(worst_case_fee_wei, params)
            ),
        ));
    } else if left_wei < worst_case_fee_wei {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!(
                "This leaves {}, less than the {} fee. You may need more funds to send again.",
                coins(left_wei, params),
                coins(worst_case_fee_wei, params)
            ),
        ));
    }
    if plan.token.is_some() && plan.should_send_max {
        warnings.push(TxWarning::new(
            TxWarningKind::Other,
            format!("Sending all tokens still requires {} for gas.", params.symbol()),
        ));
    }
    warnings
}

fn gwei(wei: u128) -> String {
    format!("{} gwei", Amount::from_base_units(wei).format(GWEI_DECIMALS))
}

fn coins(wei: u128, params: &EvmParams) -> String {
    params.chain.format_amount(Amount::from_base_units(wei))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::{BSC, ETHEREUM, POLYGON};

    #[test]
    fn rendering_uses_the_scale_of_the_unit_being_shown() {
        assert_eq!(gwei(30_000_000_000), "30 gwei");
        assert_eq!(gwei(1_250_000_000), "1.25 gwei");
        assert_eq!(gwei(0), "0 gwei");
        assert_eq!(coins(1_000_000_000_000_000_000, &ETHEREUM), "1 ETH");
        assert_eq!(coins(630_000_000_000_000, &BSC), "0.00063 BNB");
        assert_eq!(coins(11_718_000_000_000_000, &POLYGON), "0.011718 POL");
    }
}
