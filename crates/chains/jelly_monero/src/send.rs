use std::collections::BTreeMap;

use jelly_core::{SendAmount, TxRequest, UnsignedTx, random_bytes};
use monero_interface::FeeRate;
use monero_wallet::OutputWithDecoys;
use monero_wallet::ringct::RctType;
use monero_wallet::send::{Change, SendError, SignableTransaction};
use zeroize::Zeroizing;

use crate::account::MoneroAccount;
use crate::address::Recipient;
use crate::balance::MoneroBalance;
use crate::config::MoneroConfig;
use crate::decoys::{Candidate, Distribution, select_rings};
use crate::error::MoneroError;
use crate::fee::{self, Chosen, MoneroPriority};
use crate::node::MoneroNode;
use crate::params::{ASSET, piconero};
use crate::payload::UnsignedPayload;
use crate::preview;
use crate::subaddress::Subaddress;
use crate::watched::Watched;

/// Everything a quote and a build both read off a request, so the two refuse
/// exactly the same requests and a bad one fails at the quote.
#[derive(Debug)]
pub(crate) struct Request {
    pub recipient: Recipient,
    pub subaddress: Subaddress,
    pub fee: Chosen,
    /// The amount to pay, or `None` for a sweep.
    pub target_piconero: Option<u64>,
}

impl Request {
    pub(crate) fn parse(request: &TxRequest, watched: &Watched) -> Result<Self, MoneroError> {
        if request.asset != ASSET {
            return Err(MoneroError::ForeignAsset { asset: request.asset });
        }
        let address_id = request.sole_source().ok_or(MoneroError::NotOneSource)?;
        let subaddress =
            watched.subaddress_of(address_id).ok_or(MoneroError::NotWatched { address_id })?;
        let recipient = Recipient::parse(&request.to)?;
        let fee = fee::chosen_fee(&request.fee)?;
        let target_piconero = match request.amount {
            SendAmount::Max => None,
            SendAmount::Exact { value } => {
                Some(u64::try_from(value.base_units()).map_err(|_| MoneroError::AmountTooLarge)?)
            }
        };
        Ok(Self { recipient, subaddress, fee, target_piconero })
    }
}

/// Everything one build needs, gathered by the adapter under its state lock so
/// nothing here locks or reads a clock.
///
/// No `Debug`, because the candidates carry their key offsets.
pub(crate) struct Plan {
    pub recipient: Recipient,
    /// The address the payment is made from, for the preview.
    pub from: String,
    pub subaddress: Subaddress,
    /// The amount to pay, or `None` for a sweep, which cannot name its amount up
    /// front because the amount is everything minus a fee that depends on the
    /// weight.
    pub target_piconero: Option<u64>,
    /// `None` for a custom rate, which pays no priority.
    pub priority: Option<MoneroPriority>,
    pub rate: FeeRate,
    /// Unspent, unlocked, unreserved outputs of the account, largest first.
    pub candidates: Vec<Candidate>,
    /// True when the account holds more spendable outputs than one transaction
    /// may carry, so running out of candidates means the send must be split
    /// rather than that funds are short.
    pub is_truncated: bool,
    pub balance: MoneroBalance,
    /// How far behind the chain tip this wallet's scan is.
    pub scan_gap: u64,
}

/// What the assembler produced, and the figures the preview reads off it.
///
/// No `Debug`, because the signable transaction carries the outgoing view key.
pub(crate) struct Built {
    pub signable: SignableTransaction,
    pub input_count: usize,
    pub input_sum_piconero: u64,
    pub sent_piconero: u64,
    pub fee_piconero: u64,
    pub change_piconero: u64,
}

/// Rings from one node, an exact fee, a preview of the transaction that will be
/// signed, and the framed payload.
///
/// Every ring chosen goes into `chosen` by its output's key image as soon as its
/// members were asked for, whether or not the build then succeeds, because the node
/// asked has seen it either way.
pub(crate) async fn build(
    node: &dyn MoneroNode,
    config: &MoneroConfig,
    account: &MoneroAccount,
    plan: &Plan,
    distribution: &Distribution,
    chosen: &mut BTreeMap<[u8; 32], Vec<u64>>,
) -> Result<UnsignedTx, MoneroError> {
    if plan.candidates.is_empty() {
        return Err(MoneroError::InsufficientFunds {
            asset: ASSET,
            needed: piconero(plan.target_piconero.unwrap_or_default()),
            available: plan.balance.spendable(),
        });
    }
    let available = total_of(&plan.candidates, plan.candidates.len());
    if let Some(target) = plan.target_piconero
        && available < target
    {
        if plan.is_truncated && plan.balance.spendable() >= piconero(target) {
            return Err(MoneroError::TooManyInputs { maximum: config.inputs() });
        }
        return Err(MoneroError::InsufficientFunds {
            asset: ASSET,
            needed: piconero(target),
            available: piconero(available),
        });
    }

    let built = assemble(node, config, account, plan, distribution, chosen).await?;
    let preview = preview::of(config, plan, &built, distribution.height());
    let payload = UnsignedPayload {
        signable: built.signable,
        public_spend: account.public_spend(),
        build_height: distribution.height(),
        subaddress: plan.subaddress,
        endpoint: distribution.endpoint().to_owned(),
    }
    .encode()?;
    Ok(UnsignedTx { preview, payload })
}

/// Grows the input set until the exact fee is payable, fetching each new ring
/// from the one endpoint that served the output distribution.
async fn assemble(
    node: &dyn MoneroNode,
    config: &MoneroConfig,
    account: &MoneroAccount,
    plan: &Plan,
    distribution: &Distribution,
    chosen: &mut BTreeMap<[u8; 32], Vec<u64>>,
) -> Result<Built, MoneroError> {
    let change =
        Change::new(account.view_pair().clone(), plan.subaddress.account_primary().index());
    // The outgoing view key seeds this transaction's ephemeral secrets. Anyone
    // holding it can identify the transaction and read its data, so it is drawn
    // once per build and never reused. The loop below only re-prices the same
    // build, and at most one attempt becomes a transaction.
    let outgoing_view_key =
        random_bytes::<32>().map_err(|source| MoneroError::Entropy { source })?;
    let max_inputs = plan.candidates.len();
    let mut wanted = fee::inputs_for(
        plan.candidates.iter().map(|candidate| candidate.output.commitment().amount),
        plan.target_piconero,
        plan.rate,
    )
    .min(max_inputs)
    .max(1);
    let mut rings: Vec<OutputWithDecoys> = Vec::with_capacity(wanted);

    for _ in 0..=max_inputs {
        if rings.len() < wanted {
            let more = plan
                .candidates
                .get(rings.len()..wanted)
                .ok_or(MoneroError::Internal { context: "input selection went out of range" })?;
            rings.extend(select_rings(node, distribution, more, chosen).await?);
        }
        let chosen = rings
            .get(..wanted)
            .ok_or(MoneroError::Internal { context: "input selection went out of range" })?;
        let input_sum = total_of(&plan.candidates, wanted);
        match resolve_sweep(chosen, plan, input_sum, &change, &outgoing_view_key) {
            Ok(signable) => {
                let fee = signable.necessary_fee();
                let sent = plan.target_piconero.unwrap_or_else(|| input_sum.saturating_sub(fee));
                return Ok(Built {
                    signable,
                    input_count: wanted,
                    input_sum_piconero: input_sum,
                    sent_piconero: sent,
                    fee_piconero: fee,
                    change_piconero: input_sum.saturating_sub(sent).saturating_sub(fee),
                });
            }
            Err(SendError::NotEnoughFunds { .. }) if wanted < max_inputs => {
                wanted = wanted.saturating_add(1);
            }
            Err(SendError::NotEnoughFunds { inputs, necessary_fee, .. }) => {
                if plan.is_truncated {
                    return Err(MoneroError::TooManyInputs { maximum: config.inputs() });
                }
                return Err(MoneroError::InsufficientFunds {
                    asset: ASSET,
                    needed: piconero(plan.target_piconero.unwrap_or_default())
                        .saturating_add(piconero(necessary_fee.unwrap_or_default())),
                    available: piconero(inputs),
                });
            }
            Err(other) => return Err(MoneroError::Build(other)),
        }
    }
    Err(MoneroError::TooManyInputs { maximum: config.inputs() })
}

/// Assembles the signable transaction, resolving a sweep against the exact fee.
///
/// A sweep's amount is everything minus a fee that depends on the weight, and the
/// weight does not depend on the amounts, which are encrypted and fixed size. So
/// one correction resolves it, and the loop is only a bound.
fn resolve_sweep(
    inputs: &[OutputWithDecoys],
    plan: &Plan,
    input_sum: u64,
    change: &Change,
    outgoing_view_key: &Zeroizing<[u8; 32]>,
) -> Result<SignableTransaction, SendError> {
    let build_signable = |value: u64| {
        SignableTransaction::new(
            RctType::ClsagBulletproofPlus,
            outgoing_view_key.clone(),
            inputs.to_vec(),
            vec![(plan.recipient.address(), value)],
            change.clone(),
            Vec::new(),
            plan.rate,
        )
    };

    if let Some(value) = plan.target_piconero {
        return build_signable(value);
    }

    // One piconero is a valid placeholder exactly when the inputs can pay any fee
    // at all, which is exactly when a sweep is possible.
    let mut value = 1u64;
    let mut signable = build_signable(value)?;
    for _ in 0..4 {
        let fee = signable.necessary_fee();
        let swept = input_sum.checked_sub(fee).ok_or(SendError::NotEnoughFunds {
            inputs: input_sum,
            outputs: 0,
            necessary_fee: Some(fee),
        })?;
        if swept == value {
            return Ok(signable);
        }
        value = swept;
        signable = build_signable(value)?;
    }
    Ok(signable)
}

fn total_of(candidates: &[Candidate], count: usize) -> u64 {
    candidates
        .iter()
        .take(count)
        .fold(0u64, |sum, candidate| sum.saturating_add(candidate.output.commitment().amount))
}
