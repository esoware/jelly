//! The instruction rows are decoded from the bytes that will be signed rather
//! than from the request they came from.

use jelly_core::{Amount, TxDetail, TxPreview, TxWarning, TxWarningKind};
use solana_message::VersionedMessage;
use solana_system_interface::program::ID as SYSTEM_PROGRAM;

use crate::config::CHAIN;
use crate::error::SolanaError;
use crate::fee::{LAMPORTS_PER_SIGNATURE, amount_text, lamports_text, sol_text};
use crate::payload::SolanaPayload;
use crate::program;
use crate::rent::RentQuote;
use crate::route::Leg;
use crate::send::Plan;
use crate::sign;

/// A priority fee above 0.05 SOL gets a warning, the point at which Phantom warns.
const HIGH_PRIORITY_FEE_LAMPORTS: u64 = 50_000_000;

pub(crate) fn of(plan: &Plan, payload: &SolanaPayload) -> Result<TxPreview, SolanaError> {
    Ok(TxPreview {
        chain: CHAIN,
        asset: plan.route.leg.asset(),
        from: vec![plan.route.payer.to_string()],
        to: plan.route.recipient.to_string(),
        destination_tag: None,
        amount: Amount::from(plan.amount),
        fee: Amount::from(plan.fee()?),
        details: details(plan, payload)?,
        warnings: warnings(plan),
    })
}

fn details(plan: &Plan, payload: &SolanaPayload) -> Result<Vec<TxDetail>, SolanaError> {
    let mut details = vec![
        TxDetail::new("Network", "Solana mainnet-beta"),
        TxDetail::new("Fee payer", plan.route.payer.to_string()),
        TxDetail::new("Recipient", plan.route.recipient.to_string()),
        TxDetail::new("Amount", amount_text(plan.route.leg.asset(), plan.amount)),
        TxDetail::new("Recent blockhash", plan.blockhash.to_string()),
        TxDetail::new("Valid until block height", plan.expiry_height.to_string()),
        TxDetail::new("Base fee", lamports_text(LAMPORTS_PER_SIGNATURE)),
        TxDetail::new("Compute unit limit", plan.units.limit().to_string()),
        TxDetail::new("Compute unit limit source", plan.units.source().label()),
        TxDetail::new(
            "Priority price",
            format!("{} micro-lamports per compute unit", plan.pricing.micro_lamports_per_unit),
        ),
        TxDetail::new("Priority fee", lamports_text(plan.priority_fee()?)),
        TxDetail::new("Fee", lamports_text(plan.fee()?)),
    ];
    if let Some(rent) = plan.token_account_rent {
        details.push(TxDetail::new("Token account rent", lamports_text(rent.lamports())));
    }
    details.push(TxDetail::new("Total SOL debited", lamports_text(plan.lamports_out()?)));
    details.push(TxDetail::new("SOL balance", lamports_text(plan.balances.lamports)));
    token_details(plan, &mut details);
    message_details(payload, &mut details);
    Ok(details)
}

fn token_details(plan: &Plan, details: &mut Vec<TxDetail>) {
    let Leg::Token { token, source, destination } = plan.route.leg else { return };
    details.push(TxDetail::new("Mint", token.mint().to_string()));
    details.push(TxDetail::new("Token decimals", token.decimals().to_string()));
    details.push(TxDetail::new("Token program", token.program().to_string()));
    details.push(TxDetail::new("Source token account", source.to_string()));
    let suffix = if plan.route.is_account_creation() { ", created here" } else { "" };
    details.push(TxDetail::new("Destination token account", format!("{destination}{suffix}")));
    if let Some(balance) = plan.balances.token_spendable {
        details.push(TxDetail::new("Spendable token balance", amount_text(token.asset(), balance)));
    }
}

fn message_details(payload: &SolanaPayload, details: &mut Vec<TxDetail>) {
    if let VersionedMessage::V0(message) = payload.message() {
        details.push(TxDetail::new("Message version", "v0"));
        details.push(TxDetail::new("Account keys", message.account_keys.len().to_string()));
        for decoded in program::decode_message(message) {
            details.push(TxDetail::new(
                format!("Instruction {}", decoded.index),
                format!("{}: {}", decoded.program, decoded.summary),
            ));
        }
    }
    details.push(TxDetail::new(
        "Serialized size",
        format!("{} bytes", sign::signed_size(payload.message_bytes().len())),
    ));
}

fn warnings(plan: &Plan) -> Vec<TxWarning> {
    let mut warnings = Vec::new();
    if let Some(rent) = plan.token_account_rent {
        warnings.push(TxWarning::new(
            TxWarningKind::AccountActivation,
            format!(
                "This creates the recipient's token account with a {} rent deposit. Only the recipient can reclaim it.",
                lamports_text(rent.lamports())
            ),
        ));
    }
    if plan.account_rent.is_ceiling() || plan.token_account_rent.is_some_and(RentQuote::is_ceiling)
    {
        warnings.push(TxWarning::new(
            TxWarningKind::Estimated,
            "Rent could not be checked. This estimate uses Jelly's fallback ceiling.",
        ));
    }
    if plan.units.is_fallback() {
        warnings.push(TxWarning::new(
            TxWarningKind::Estimated,
            "Simulation failed. A fallback compute limit determines the priority fee.",
        ));
    }
    if let Ok(lamports) = plan.priority_fee()
        && lamports > HIGH_PRIORITY_FEE_LAMPORTS
    {
        warnings.push(TxWarning::new(
            TxWarningKind::HighFee,
            format!(
                "The {} priority fee is unusually high. Check it before signing.",
                lamports_text(lamports)
            ),
        ));
    }
    if plan.pricing.is_capped {
        warnings.push(TxWarning::new(
            TxWarningKind::HighFee,
            "The quoted priority fee exceeds Jelly's limit. This estimate uses the limit.",
        ));
    }
    if matches!(plan.route.leg, Leg::Native) {
        recipient_warnings(plan, &mut warnings);
    }
    rent_warnings(plan, &mut warnings);
    warnings
}

/// A token transfer's recipient was already settled by the build, which refuses
/// one tokens could not safely reach. Coin goes to the address itself, so these
/// are warnings.
///
/// A wallet address is owned by the system program. Lamports sent to an account
/// any other program owns are lost unless that program can move them, and
/// sending SOL to a token account is the classic way to lose it. A
/// program-derived address is off the curve, and coin sent to one is recoverable
/// only if the owning program allows it. A program can be sent coin on purpose,
/// so neither is a refusal.
fn recipient_warnings(plan: &Plan, warnings: &mut Vec<TxWarning>) {
    if let Some(owner) = plan.recipient_program.filter(|owner| *owner != SYSTEM_PROGRAM) {
        warnings.push(TxWarning::new(
            TxWarningKind::AmbiguousAddress,
            format!(
                "Program {owner} controls this recipient account. You may lose access to funds sent here unless the program allows withdrawals."
            ),
        ));
    }
    if !plan.route.recipient.is_on_curve() {
        warnings.push(TxWarning::new(
            TxWarningKind::AmbiguousAddress,
            "This address has no private key and may belong to a program. Funds are recoverable only if that program allows withdrawals.",
        ));
    }
}

/// Rent is no longer collected, but a transaction that would leave a nonzero
/// balance below the rent-exempt minimum fails, and an emptied account is
/// removed.
fn rent_warnings(plan: &Plan, warnings: &mut Vec<TxWarning>) {
    let is_native = matches!(plan.route.leg, Leg::Native);
    let remainder = plan.remainder();
    if plan.should_send_max && is_native {
        warnings.push(TxWarning::new(
            TxWarningKind::RentExemption,
            "This empties the account. Solana may remove it, but you can fund the same address again.",
        ));
    } else if remainder > 0 && remainder < plan.account_rent.lamports() {
        warnings.push(TxWarning::new(
            TxWarningKind::RentExemption,
            format!(
                "This leaves {}, below the {} rent minimum. The network will reject this transfer.",
                sol_text(remainder),
                sol_text(plan.account_rent.lamports())
            ),
        ));
    }
    if is_native
        && !plan.route.has_destination_account
        && plan.amount < plan.account_rent.lamports()
    {
        warnings.push(TxWarning::new(
            TxWarningKind::RentExemption,
            format!(
                "The recipient needs a new account. The network will reject {} because it is below the {} rent minimum.",
                sol_text(plan.amount),
                sol_text(plan.account_rent.lamports())
            ),
        ));
    }
}
