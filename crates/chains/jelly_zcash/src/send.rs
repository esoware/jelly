use jelly_core::{AssetId, Clock, FeeSelection, SendAmount, TxRequest};
use zcash_primitives::transaction::fees::zip317::{GRACE_ACTIONS, MARGINAL_FEE};
use zcash_protocol::value::MAX_MONEY;

use crate::address::script;
use crate::client::ZcashClient;
use crate::config::{CHAIN, DUST_ZAT, MAX_INPUTS};
use crate::entry::ENTRY;
use crate::error::ZcashError;
use crate::fee::{conventional_fee, paid_fee};
use crate::payload::{Coin, Payment, Source};
use crate::state::States;
use crate::watched::Watched;
use crate::zat::add;

const UNSUPPORTED_DESTINATION: &str =
    "Enter a Zcash transparent mainnet address. Shielded and unified addresses are not supported.";
const CHOOSE_SOURCE: &str = "Choose a Zcash source address.";

pub(crate) struct Inputs<'a> {
    pub(crate) client: &'a ZcashClient,
    pub(crate) states: &'a States,
    pub(crate) clock: &'a dyn Clock,
    pub(crate) watched: &'a [Watched],
}

#[derive(Debug)]
struct Selection {
    inputs: Vec<Coin>,
    amount_zat: u64,
    change_zat: u64,
    fee_zat: u64,
    dropped_change_zat: u64,
}

/// A payment resolved against a fresh tip.
pub(crate) struct Resolved {
    pub(crate) payment: Payment,
    /// What the normal level would pay over the same coins, or `None` when the coins
    /// can't pay it.
    pub(crate) normal_fee_zat: Option<u64>,
}

/// The fee `selection` names in zatoshis, or `None` for the level, which pays the ZIP
/// 317 fee. Another chain's custom fee is refused rather than ignored.
fn chosen_fee_zat(selection: &FeeSelection) -> Result<Option<u64>, ZcashError> {
    match *selection {
        FeeSelection::Level { .. } => Ok(None),
        FeeSelection::Total { fee } => u64::try_from(fee.base_units())
            .ok()
            .filter(|zat| *zat <= MAX_MONEY)
            .map(Some)
            .ok_or(ZcashError::Invalid("Enter a smaller Zcash fee.")),
        FeeSelection::Rate { .. } | FeeSelection::Evm { .. } | FeeSelection::Solana { .. } => {
            Err(ZcashError::Invalid("Choose a Zcash fee level or a fee in ZEC."))
        }
    }
}

/// The payment `request` asks for, built on a fresh tip from the coins of its sources
/// that no held payment spends. Quotes and builds share it, so the user approves the
/// numbers the payment is built with.
pub(crate) async fn resolve(
    inputs: &Inputs<'_>,
    request: &TxRequest,
) -> Result<Resolved, ZcashError> {
    let Inputs { client, states, clock, watched } = *inputs;
    states.ensure_readable()?;
    if request.asset != AssetId::Native(CHAIN) || request.destination_tag.is_some() {
        return Err(ZcashError::Invalid("Choose Zcash without a destination tag."));
    }
    let chosen_zat = chosen_fee_zat(&request.fee)?;
    let sources = sources(watched, &request.from_address_ids)?;
    let Some(change) = sources.first() else {
        return Err(ZcashError::Invalid(CHOOSE_SOURCE));
    };
    let to = ENTRY
        .validate_address(&request.to)
        .map_err(|_| ZcashError::Invalid(UNSUPPORTED_DESTINATION))?
        .into_value();
    let tip = client.fresh_tip(clock).await?;
    let target_height = tip.height.checked_add(1).ok_or(ZcashError::Protocol)?;
    let mut coins = Vec::new();
    for source in &sources {
        coins.extend(client.coins(&source.address).await?);
    }
    coins.retain(|coin| !states.is_reserved(&coin.txid, coin.index));
    let normal_fee_zat = match chosen_zat {
        None => None,
        Some(_) => select(coins.clone(), request.amount, target_height, &to, &change.address, None)
            .ok()
            .map(|normal| normal.fee_zat),
    };
    let selection = select(coins, request.amount, target_height, &to, &change.address, chosen_zat)?;
    let moved: Vec<Source> = sources
        .into_iter()
        .enumerate()
        .filter(|(position, source)| {
            (*position == 0 && selection.change_zat > 0)
                || selection.inputs.iter().any(|coin| coin.address == source.address)
        })
        .map(|(_, source)| source)
        .collect();
    let payment = Payment {
        sources: moved,
        to,
        target_height,
        expiry_height: Payment::expiry_height(target_height)?,
        inputs: selection.inputs,
        amount_zat: selection.amount_zat,
        change_zat: selection.change_zat,
        fee_zat: selection.fee_zat,
        dropped_change_zat: selection.dropped_change_zat,
    };
    payment.validate()?;
    let normal_fee_zat = match chosen_zat {
        None => Some(payment.fee_zat),
        Some(_) => normal_fee_zat,
    };
    Ok(Resolved { payment, normal_fee_zat })
}

/// The watched addresses of `address_ids`, in their order, each once.
fn sources(watched: &[Watched], address_ids: &[i64]) -> Result<Vec<Source>, ZcashError> {
    let mut sources: Vec<Source> = Vec::with_capacity(address_ids.len());
    for &address_id in address_ids {
        let entry = watched
            .iter()
            .find(|entry| entry.address_id == address_id)
            .ok_or(ZcashError::Invalid(CHOOSE_SOURCE))?;
        if !sources.iter().any(|source| source.address_id == address_id) {
            sources.push(Source { address_id, address: entry.address.clone() });
        }
    }
    Ok(sources)
}

/// Picks coins largest first, with change to `change_address`, at the ZIP 317 fee or the
/// custom fee `chosen_zat`.
fn select(
    mut coins: Vec<Coin>,
    amount: SendAmount,
    target_height: u32,
    to: &str,
    change_address: &str,
    chosen_zat: Option<u64>,
) -> Result<Selection, ZcashError> {
    coins.sort_by(|left, right| {
        right
            .value_zat
            .cmp(&left.value_zat)
            .then_with(|| left.txid.cmp(&right.txid))
            .then(left.index.cmp(&right.index))
    });
    let wanted_zat = match amount {
        SendAmount::Exact { value } => Some(
            u64::try_from(value.base_units())
                .ok()
                .filter(|value| *value <= MAX_MONEY)
                .ok_or(ZcashError::Invalid("Enter a smaller Zcash amount."))?,
        ),
        SendAmount::Max => None,
    };
    if let Some(wanted_zat) = wanted_zat
        && wanted_zat < DUST_ZAT
    {
        return Err(ZcashError::Dust { amount_zat: wanted_zat });
    }
    let recipient = script(to)?;
    let change = script(change_address)?;
    let with_change = [&recipient, &change];
    let mut inputs = Vec::new();
    let mut total_zat = 0;
    for coin in coins {
        // Past the grace actions each input adds the marginal fee, so a coin worth no more
        // than that adds nothing to the payment.
        if inputs.len() >= GRACE_ACTIONS && coin.value_zat <= u64::from(MARGINAL_FEE) {
            continue;
        }
        if inputs.len() == MAX_INPUTS {
            return Err(ZcashError::Invalid(
                "These addresses have too many inputs for one payment. Send a smaller amount \
                 first.",
            ));
        }
        total_zat = add(total_zat, coin.value_zat)?;
        inputs.push(coin);
        if let Some(wanted_zat) = wanted_zat {
            let fee_zat = conventional_fee(target_height, &inputs, &with_change)?;
            if total_zat
                >= add(wanted_zat, chosen_zat.map_or(fee_zat, |chosen| chosen.max(fee_zat)))?
            {
                break;
            }
        }
    }
    let Some(wanted_zat) = wanted_zat else {
        let fee_zat =
            paid_fee(conventional_fee(target_height, &inputs, &[&recipient])?, chosen_zat)?;
        let amount_zat = total_zat.saturating_sub(fee_zat);
        let needed_zat = add(DUST_ZAT, fee_zat)?;
        if total_zat < needed_zat {
            return Err(ZcashError::Funds { needed_zat, available_zat: total_zat });
        }
        return Ok(Selection { inputs, amount_zat, change_zat: 0, fee_zat, dropped_change_zat: 0 });
    };
    let fee_zat = paid_fee(conventional_fee(target_height, &inputs, &with_change)?, chosen_zat)?;
    let needed_zat = add(wanted_zat, fee_zat)?;
    if total_zat < needed_zat {
        return Err(ZcashError::Funds { needed_zat, available_zat: total_zat });
    }
    let change_zat = total_zat
        .checked_sub(needed_zat)
        .ok_or(ZcashError::Internal("the Zcash change is negative"))?;
    if change_zat < DUST_ZAT {
        return Ok(Selection {
            inputs,
            amount_zat: wanted_zat,
            change_zat: 0,
            fee_zat: add(fee_zat, change_zat)?,
            dropped_change_zat: change_zat,
        });
    }
    Ok(Selection { inputs, amount_zat: wanted_zat, change_zat, fee_zat, dropped_change_zat: 0 })
}
