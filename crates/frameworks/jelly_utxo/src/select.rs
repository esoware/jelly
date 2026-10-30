use std::cmp::Reverse;
use std::slice::from_ref;

use bitcoin::{Script, Weight};
use jelly_core::{Amount, FeeRate, wire_enum};

use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::script::SpendableScript;
use crate::utxo::{Utxo, milli};
use crate::{fee, weight};

/// Caps the changeless search, which is exponential in the worst case, so a large
/// unspent set can't hang the wallet.
const MAX_BNB_ROUNDS: usize = 100_000;

wire_enum! {
    /// Which rule produced a selection, recorded in the payload.
    pub(crate) enum Strategy {
        /// An exact match inside the cost-of-change window, so no change output.
        Changeless => "changeless",
        /// The fewest coins that pay, plus any further confirmed coins that lower the
        /// waste.
        LowestWaste => "lowest_waste",
        /// Everything spendable, for a send-max.
        Drain => "drain",
        /// A replacement reusing the replaced transaction's inputs.
        Replacement => "replacement",
        /// A child spending its parent's change, with more coins only as needed.
        ChildPaysForParent => "child_pays_for_parent",
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct SelectionRequest<'a> {
    /// What the recipient must receive. Ignored when `should_drain` is set.
    pub(crate) target: Amount,
    pub(crate) should_drain: bool,
    pub(crate) fee_rate: FeeRate,
    /// The rate a change output is assumed to cost to spend later. It drives the
    /// waste metric, decides whether a small leftover is worth paying back, and
    /// caps how far a cheaper rate consolidates.
    pub(crate) long_term_fee_rate: FeeRate,
    pub(crate) recipient_script_pubkey: &'a Script,
    /// The first source, since Jelly has no change keychain.
    pub(crate) change_script: &'a SpendableScript,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    pub(crate) inputs: Vec<Utxo>,
    /// What the recipient receives. Equals the request's target except when
    /// draining.
    pub(crate) recipient_value: Amount,
    pub(crate) change: Option<Amount>,
    /// Leftover too small to be worth a change output. It goes to the miner and is
    /// included in `fee`.
    pub(crate) donated: Amount,
    pub(crate) fee: Amount,
    /// Predicted weight of the signed transaction.
    pub(crate) weight: Weight,
    pub(crate) strategy: Strategy,
}

impl Selection {
    #[cfg(test)]
    fn input_total(&self) -> Result<Amount, UtxoError> {
        Utxo::total(&self.inputs)
    }

    #[cfg(test)]
    fn output_total(&self) -> Result<Amount, UtxoError> {
        self.recipient_value
            .checked_add(self.change.unwrap_or(Amount::ZERO))
            .ok_or_else(|| UtxoError::overflow("an output total"))
    }
}

/// Which coins a selection may spend before they confirm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Eligible {
    Confirmed,
    /// Confirmed coins and the wallet's own unconfirmed change, Bitcoin Core's
    /// trusted coins. Someone else's unconfirmed payment waits until it confirms.
    Trusted,
}

/// The `eligible` coins worth spending at `rate`, by descending effective value, then
/// by outpoint.
///
/// Confirmation doesn't affect the order. [`select_coins`] already tries confirmed
/// coins alone first, and sorting a large mempool coin behind many small confirmed
/// ones could leave every set that reaches it too heavy. The bump planners use the
/// same order, so the inputs they add stay predictable.
pub(crate) fn ordered_candidates(coins: &[Utxo], rate: FeeRate, eligible: Eligible) -> Vec<Utxo> {
    let mut candidates: Vec<Utxo> = coins
        .iter()
        .filter(|coin| match eligible {
            Eligible::Confirmed => coin.is_confirmed(),
            Eligible::Trusted => coin.is_trusted(),
        })
        // Costs more to spend than it's worth at this rate.
        .filter(|coin| coin.effective_milli(rate) > 0)
        .cloned()
        .collect();
    candidates.sort_by_key(|coin| (Reverse(coin.effective_milli(rate)), coin.outpoint()));
    candidates
}

/// Tries confirmed coins alone first and adds the wallet's own unconfirmed change only
/// when they can't pay, because unconfirmed change carries reorg risk and a BIP-125
/// replacement can't add it later.
///
/// Send-max spends every trusted candidate instead.
pub(crate) fn select_coins(
    params: &UtxoParams,
    request: &SelectionRequest<'_>,
    coins: &[Utxo],
) -> Result<Selection, UtxoError> {
    if request.target > params.max_money {
        return Err(UtxoError::AboveMaxMoney {
            value: request.target,
            maximum: params.max_money,
            chain: params.chain,
        });
    }
    if request.should_drain {
        let candidates = ordered_candidates(coins, request.fee_rate, Eligible::Trusted);
        return drain(params, request, &candidates);
    }
    let confirmed = ordered_candidates(coins, request.fee_rate, Eligible::Confirmed);
    let first = from_candidates(params, request, &confirmed);
    if first.is_ok() {
        return first;
    }
    let all = ordered_candidates(coins, request.fee_rate, Eligible::Trusted);
    if all.len() == confirmed.len() {
        return first;
    }
    from_candidates(params, request, &all)
}

fn from_candidates(
    params: &UtxoParams,
    request: &SelectionRequest<'_>,
    candidates: &[Utxo],
) -> Result<Selection, UtxoError> {
    let change_floor =
        fee::change_floor(params, request.long_term_fee_rate, request.change_script)?;
    let (count, priced) = fewest(params, request, candidates, change_floor)?;
    let cap = weight_cap(params, request, priced.weight);
    if let Some(found) = changeless(request, candidates, cap, change_floor)? {
        return Ok(found);
    }
    accumulate(request, candidates, count, priced, cap, change_floor)
}

/// The length of the shortest run from the front of the ordered candidates that
/// pays, with its price.
///
/// The candidates are sorted by effective value, so no smaller set pays. With one
/// script kind no lighter set pays either, and mixed kinds only move that bound by the
/// difference between their input weights.
///
/// Fails for weight when only a run too heavy to relay pays, and for funds when even
/// every candidate together can't.
fn fewest(
    params: &UtxoParams,
    request: &SelectionRequest<'_>,
    candidates: &[Utxo],
    change_floor: Amount,
) -> Result<(usize, Priced), UtxoError> {
    let Some(mut priced) = price(request, candidates, change_floor)? else {
        return Err(insufficient(request, candidates)?);
    };
    let mut input_wu = 0u64;
    let mut too_few = 0usize;
    for (count, coin) in (1..).zip(candidates) {
        input_wu = input_wu.saturating_add(coin.input_weight().to_wu());
        if input_wu > params.max_standard_weight.to_wu() {
            break;
        }
        let Some(inputs) = candidates.get(..count) else { break };
        if let Some(found) = price(request, inputs, change_floor)? {
            weight::check_standard_weight(params, found.weight)?;
            return Ok((count, found));
        }
        too_few = count;
    }
    // The error names the weight of the shortest run that pays. Every candidate adds
    // more than its input costs, so apart from rounding a longer run pays whenever a
    // shorter one does, and a binary search finds it without pricing every run.
    let mut count = candidates.len();
    while count.saturating_sub(too_few) > 1 {
        let middle = too_few.midpoint(count);
        let Some(inputs) = candidates.get(..middle) else { break };
        match price(request, inputs, change_floor)? {
            Some(found) => {
                count = middle;
                priced = found;
            }
            None => too_few = middle,
        }
    }
    Err(weight::too_heavy(params, priced.weight))
}

/// The heaviest selection worth making, given the weight of the lightest set that
/// pays.
///
/// Below the long-term rate the waste metric rewards every extra input, so
/// consolidation stops at the weight whose fee at this rate matches the lightest
/// set's fee at the long-term rate. Without the cap, a low rate could quote many
/// times the fee of a higher one.
fn weight_cap(params: &UtxoParams, request: &SelectionRequest<'_>, lightest: Weight) -> Weight {
    let now_per_kwu = request.fee_rate.sat_per_kwu();
    let later_per_kwu = request.long_term_fee_rate.sat_per_kwu();
    if now_per_kwu >= later_per_kwu {
        return params.max_standard_weight;
    }
    let affordable_wu =
        lightest.to_wu().saturating_mul(later_per_kwu).checked_div(now_per_kwu).unwrap_or(u64::MAX);
    Weight::from_wu(affordable_wu).min(params.max_standard_weight)
}

/// Branch and bound for a changeless selection no heavier than `cap`.
///
/// A changeless transaction is smaller, leaves no coin to spend later, and reveals no
/// change amount, so it wins whenever one fits the cost-of-change window. The search
/// only picks coins, and [`price`] re-checks every set it would keep.
fn changeless(
    request: &SelectionRequest<'_>,
    candidates: &[Utxo],
    cap: Weight,
    change_floor: Amount,
) -> Result<Option<Selection>, UtxoError> {
    let Some(first) = candidates.first() else {
        return Ok(None);
    };
    if request.target.is_zero() {
        return Ok(None);
    }
    let rate_per_kwu = i128::from(request.fee_rate.sat_per_kwu());
    let later_per_kwu = i128::from(request.long_term_fee_rate.sat_per_kwu());
    // Uses the first coin's overhead, rounded to whole vbytes, for every selection.
    // That's a few wu off for mixed legacy/segwit inputs, odd P2TR counts, and 253+
    // inputs, which is fine since `price` re-checks every set the search keeps.
    let overhead_wu =
        plain_weight(request, from_ref(first)).to_wu().saturating_sub(first.input_weight().to_wu());
    let charged_overhead_wu =
        weight::vsize(Weight::from_wu(overhead_wu)).saturating_mul(Weight::WITNESS_SCALE_FACTOR);

    let target_milli = milli(request.target)?
        .checked_add(rate_per_kwu.saturating_mul(i128::from(charged_overhead_wu)))
        .ok_or_else(|| UtxoError::overflow("a selection target"))?;
    // `price` makes change only from what's left after the change output's own fee,
    // so the window extends past the change floor by that fee.
    let change_output_wu =
        i128::from(weight::output_weight(request.change_script.script_pubkey()).to_wu());
    let limit_milli = milli(change_floor)?
        .checked_add(rate_per_kwu.saturating_mul(change_output_wu))
        .ok_or_else(|| UtxoError::overflow("the changeless window"))?;

    let coins: Vec<Modeled> = candidates
        .iter()
        .map(|coin| {
            let input_wu = coin.input_weight().to_wu();
            Modeled {
                effective_milli: coin.effective_milli(request.fee_rate),
                input_wu,
                timing_milli: i128::from(input_wu)
                    .saturating_mul(rate_per_kwu.saturating_sub(later_per_kwu)),
            }
        })
        .collect();
    let bounds = Bounds {
        target_milli,
        limit_milli,
        max_input_wu: cap.to_wu().saturating_sub(overhead_wu),
        is_rate_high: rate_per_kwu > later_per_kwu,
    };
    search(&coins, &bounds, |picked| {
        let inputs: Vec<Utxo> =
            picked.iter().filter_map(|index| candidates.get(*index)).cloned().collect();
        let Some(priced) = price(request, &inputs, change_floor)? else {
            return Ok(None);
        };
        if priced.change.is_some() || priced.weight > cap {
            return Ok(None);
        }
        Ok(Some(Selection {
            inputs,
            recipient_value: request.target,
            change: None,
            donated: priced.donated,
            fee: priced.fee,
            weight: priced.weight,
            strategy: Strategy::Changeless,
        }))
    })
}

/// One candidate as the changeless search models it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Modeled {
    effective_milli: i128,
    input_wu: u64,
    /// What spending this input now costs over spending it at the long-term rate.
    timing_milli: i128,
}

/// What a modeled selection has to satisfy.
#[derive(Debug)]
struct Bounds {
    /// The effective value that pays the recipient and the rest of the plain
    /// transaction.
    target_milli: i128,
    /// The excess over `target_milli` at which [`price`] makes change instead.
    limit_milli: i128,
    max_input_wu: u64,
    /// Above the long-term rate every input adds waste, so a partial selection
    /// already more wasteful than the best one is abandoned.
    is_rate_high: bool,
}

/// The least-waste changeless selection `settle` accepts, after Bitcoin Core's
/// `SelectCoinsBnB`.
///
/// A depth-first search that tries including each coin before excluding it. The model
/// only sums effective values, and saturating sums are fine since `settle` checks each
/// set against [`price`]. A set `settle` refuses is skipped and the search goes on. A
/// coin identical to the one just excluded isn't included in its place, since that
/// branch was already explored.
fn search(
    coins: &[Modeled],
    bounds: &Bounds,
    mut settle: impl FnMut(&[usize]) -> Result<Option<Selection>, UtxoError>,
) -> Result<Option<Selection>, UtxoError> {
    let mut available_milli =
        coins.iter().fold(0i128, |sum, coin| sum.saturating_add(coin.effective_milli));
    let mut selected: Vec<usize> = Vec::new();
    let mut value_milli = 0i128;
    let mut input_wu = 0u64;
    let mut timing_milli = 0i128;
    let mut best: Option<(i128, Selection)> = None;
    let mut index = 0usize;
    for _ in 0..MAX_BNB_ROUNDS {
        let excess_milli = value_milli.saturating_sub(bounds.target_milli);
        let is_wasteful =
            bounds.is_rate_high && best.as_ref().is_some_and(|(least, _)| timing_milli > *least);
        let should_backtrack = if excess_milli.saturating_add(available_milli) < 0
            || excess_milli >= bounds.limit_milli
            || input_wu > bounds.max_input_wu
            || is_wasteful
        {
            true
        } else if excess_milli >= 0 {
            let waste_milli = timing_milli.saturating_add(excess_milli);
            if best.as_ref().is_none_or(|(least, _)| waste_milli < *least)
                && let Some(settled) = settle(&selected)?
            {
                best = Some((waste_milli, settled));
            }
            true
        } else {
            false
        };

        if should_backtrack {
            let Some(last) = selected.pop() else { break };
            let Some(coin) = coins.get(last) else { break };
            let next = last.saturating_add(1);
            // The coins considered after `last` are candidates again once it's excluded.
            let passed = coins.get(next..index).unwrap_or_default();
            available_milli = passed
                .iter()
                .fold(available_milli, |sum, passed| sum.saturating_add(passed.effective_milli));
            value_milli = value_milli.saturating_sub(coin.effective_milli);
            input_wu = input_wu.saturating_sub(coin.input_wu);
            timing_milli = timing_milli.saturating_sub(coin.timing_milli);
            index = next;
        } else {
            let Some(coin) = coins.get(index) else { break };
            available_milli = available_milli.saturating_sub(coin.effective_milli);
            let is_twin_of_excluded = index.checked_sub(1).is_some_and(|before| {
                selected.last() != Some(&before) && coins.get(before) == Some(coin)
            });
            if !is_twin_of_excluded {
                selected.push(index);
                value_milli = value_milli.saturating_add(coin.effective_milli);
                input_wu = input_wu.saturating_add(coin.input_wu);
                timing_milli = timing_milli.saturating_add(coin.timing_milli);
            }
            index = index.saturating_add(1);
        }
    }
    Ok(best.map(|(_, selection)| selection))
}

/// Adds further confirmed coins to the first `count` candidates, which pay at
/// `priced`, up to `cap`, and keeps the set with the least waste.
///
/// An unconfirmed coin is never added just to consolidate, since each one lengthens a
/// mempool chain that relay policy limits.
fn accumulate(
    request: &SelectionRequest<'_>,
    candidates: &[Utxo],
    count: usize,
    priced: Priced,
    cap: Weight,
    change_floor: Amount,
) -> Result<Selection, UtxoError> {
    let (needed, rest) = candidates.split_at_checked(count).unwrap_or((candidates, &[]));
    let mut inputs = needed.to_vec();
    let mut best = (waste(request, &inputs, &priced)?, inputs.len(), priced);
    for coin in rest.iter().filter(|coin| coin.is_confirmed()) {
        inputs.push(coin.clone());
        let Some(priced) = price(request, &inputs, change_floor)? else { continue };
        if priced.weight > cap {
            break;
        }
        let score = waste(request, &inputs, &priced)?;
        let has_change = priced.change.is_some();
        if score < best.0 {
            best = (score, inputs.len(), priced);
        }
        // From here at this rate every input only adds waste.
        if has_change && request.fee_rate >= request.long_term_fee_rate {
            break;
        }
    }
    let (_, count, priced) = best;
    inputs.truncate(count);
    Ok(Selection {
        inputs,
        recipient_value: request.target,
        change: priced.change,
        donated: priced.donated,
        fee: priced.fee,
        weight: priced.weight,
        strategy: Strategy::LowestWaste,
    })
}

/// Reports the shortfall against this tier's eligible coins, not the full
/// balance.
///
/// Excluded or uneconomic coins can't fund the payment. The fee assumes every
/// candidate is spent, since each adds more than it costs.
fn insufficient(
    request: &SelectionRequest<'_>,
    candidates: &[Utxo],
) -> Result<UtxoError, UtxoError> {
    let available = Utxo::total(candidates)?;
    let fee = fee::fee_for(request.fee_rate, plain_weight(request, candidates))?;
    let needed = request
        .target
        .checked_add(fee)
        .ok_or_else(|| UtxoError::overflow("the amount a payment needs"))?;
    Ok(UtxoError::InsufficientFunds { needed, available })
}

/// The waste metric, in thousandths of a base unit so nothing has to divide.
///
/// The timing cost of spending these inputs now rather than at the long-term
/// rate, plus the cost of change, or the donated leftover when there is no
/// change.
fn waste(
    request: &SelectionRequest<'_>,
    inputs: &[Utxo],
    priced: &Priced,
) -> Result<i128, UtxoError> {
    let mut input_wu: i128 = 0;
    for coin in inputs {
        input_wu = input_wu
            .checked_add(i128::from(coin.input_weight().to_wu()))
            .ok_or_else(|| UtxoError::overflow("an input weight"))?;
    }
    let now_per_kwu = i128::from(request.fee_rate.sat_per_kwu());
    let later_per_kwu = i128::from(request.long_term_fee_rate.sat_per_kwu());
    let timing_milli = input_wu
        .checked_mul(now_per_kwu.saturating_sub(later_per_kwu))
        .ok_or_else(|| UtxoError::overflow("a timing cost"))?;

    let change_term = if priced.change.is_some() {
        fee::cost_of_change(request.fee_rate, request.long_term_fee_rate, request.change_script)?
    } else {
        priced.donated
    };
    timing_milli
        .checked_add(milli(change_term)?)
        .ok_or_else(|| UtxoError::overflow("the waste metric"))
}

struct Priced {
    change: Option<Amount>,
    donated: Amount,
    fee: Amount,
    weight: Weight,
}

/// Prices one input set with and without change, keeping change when it reaches
/// `change_floor`. `None` when the set can't pay.
fn price(
    request: &SelectionRequest<'_>,
    inputs: &[Utxo],
    change_floor: Amount,
) -> Result<Option<Priced>, UtxoError> {
    let total = Utxo::total(inputs)?;

    let with_change = weight::predict_weight(
        inputs.iter().map(Utxo::kind),
        [request.recipient_script_pubkey, request.change_script.script_pubkey()],
    );
    let fee_with_change = fee::fee_for(request.fee_rate, with_change)?;
    if let Some(change) =
        total.checked_sub(request.target).and_then(|left| left.checked_sub(fee_with_change))
        && change >= change_floor
    {
        return Ok(Some(Priced {
            change: Some(change),
            donated: Amount::ZERO,
            fee: fee_with_change,
            weight: with_change,
        }));
    }

    // Not worth a change output, so the leftover goes to the miner and the preview
    // says so.
    let plain = plain_weight(request, inputs);
    let fee_plain = fee::fee_for(request.fee_rate, plain)?;
    let Some(donated) =
        total.checked_sub(request.target).and_then(|left| left.checked_sub(fee_plain))
    else {
        return Ok(None);
    };
    let fee = fee_plain.checked_add(donated).ok_or_else(|| UtxoError::overflow("a fee"))?;
    Ok(Some(Priced { change: None, donated, fee, weight: plain }))
}

/// Send-max, spending every candidate into one output with no change.
///
/// Nothing to spend fails the same arithmetic as too little to cover the fee, so
/// there's no separate empty case.
fn drain(
    params: &UtxoParams,
    request: &SelectionRequest<'_>,
    candidates: &[Utxo],
) -> Result<Selection, UtxoError> {
    let total = Utxo::total(candidates)?;
    let weight = plain_weight(request, candidates);
    let fee = fee::fee_for(request.fee_rate, weight)?;
    let paid = total.checked_sub(fee).ok_or_else(|| UtxoError::InsufficientFunds {
        needed: fee.saturating_add(Amount::from_base_units(1)),
        available: total,
    })?;
    let dust = params.dust_threshold(request.recipient_script_pubkey);
    if paid < dust {
        return Err(UtxoError::Dust { value: paid, threshold: dust, chain: params.chain });
    }
    Ok(Selection {
        inputs: candidates.to_vec(),
        recipient_value: paid,
        change: None,
        donated: Amount::ZERO,
        fee,
        weight,
        strategy: Strategy::Drain,
    })
}

/// Weight of the signed transaction paying `inputs` to the recipient alone.
fn plain_weight(request: &SelectionRequest<'_>, inputs: &[Utxo]) -> Weight {
    weight::predict_weight(inputs.iter().map(Utxo::kind), [request.recipient_script_pubkey])
}

#[cfg(test)]
mod tests {
    use bitcoin::OutPoint;
    use jelly_core::ScriptKind;
    use proptest::{prelude::*, test_runner::RngSeed};

    use super::*;
    use crate::fee::{change_floor, fee_for};
    use crate::fixtures::{
        ONE_PER_VB, Scene, TEN_PER_VB, TIP, bitcoin_scene, confirmed, incoming, litecoin_scene,
        scene, unconfirmed,
    };
    use crate::script::UtxoScript;
    use crate::select::Strategy;
    use crate::test_util::{BITCOIN, fixture_txid};
    use crate::utxo::{Origin, units};
    use crate::weight::{input_weight, predict_weight};

    const PROPERTY_SEED: u64 = 20_261_021;

    const FIFTY_PER_VB: FeeRate = FeeRate::from_sat_per_kwu(12_500);

    /// A seed byte names at most 255 fixture coins, so these share one txid and
    /// differ by output index.
    fn many_coins(scene: &Scene, count: u32, value: u64) -> Vec<Utxo> {
        let txid = fixture_txid(250);
        (0..count)
            .map(|vout| {
                Utxo::new(
                    OutPoint { txid, vout },
                    units(value),
                    scene.ours.script().clone(),
                    Some(TIP),
                    Origin::Ordinary,
                )
            })
            .collect()
    }

    #[test]
    fn selection_is_deterministic_whatever_order_the_server_listed_the_coins_in() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[40_000, 90_000, 10_000, 60_000]);
        let mut shuffled = coins.clone();
        shuffled.reverse();

        let request = scene.selection(50_000, ONE_PER_VB);
        let first = scene.select(&request, &coins).unwrap();
        let second = scene.select(&request, &shuffled).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn candidates_are_ordered_by_descending_effective_value_then_outpoint() {
        let scene = bitcoin_scene().unwrap();
        let coins = vec![
            confirmed(&scene.ours, 1, 10_000),
            unconfirmed(&scene.ours, 2, 500_000),
            confirmed(&scene.ours, 3, 90_000),
        ];
        let ordered = ordered_candidates(&coins, ONE_PER_VB, Eligible::Trusted);
        let values: Vec<Amount> = ordered.iter().map(Utxo::value).collect();
        assert_eq!(values, vec![units(500_000), units(90_000), units(10_000)]);

        let confirmed_only = ordered_candidates(&coins, ONE_PER_VB, Eligible::Confirmed);
        assert_eq!(confirmed_only.len(), 2);
        assert!(confirmed_only.iter().all(Utxo::is_confirmed));

        let tied = [confirmed(&scene.ours, 9, 10_000), confirmed(&scene.ours, 8, 10_000)];
        let ordered = ordered_candidates(&tied, ONE_PER_VB, Eligible::Confirmed);
        assert_eq!(
            ordered.iter().map(Utxo::outpoint).collect::<Vec<_>>(),
            [tied[1].outpoint(), tied[0].outpoint()]
        );
    }

    #[test]
    fn a_coin_with_no_positive_effective_value_is_not_a_candidate() {
        let scene = bitcoin_scene().unwrap();
        // A P2WPKH input weighs 272 wu, which costs 680 base units at ten per vB.
        let coins = vec![confirmed(&scene.ours, 1, 500)];
        assert!(ordered_candidates(&coins, TEN_PER_VB, Eligible::Trusted).is_empty());
        assert_eq!(ordered_candidates(&coins, ONE_PER_VB, Eligible::Trusted).len(), 1);
    }

    /// A mempool coin adds reorg risk and runs into BIP-125 rule 2 later, so it's
    /// never spent while confirmed coins would do.
    #[test]
    fn confirmed_coins_alone_are_tried_before_any_mempool_coin_is_admitted() {
        let scene = bitcoin_scene().unwrap();
        let coins = vec![unconfirmed(&scene.ours, 1, 1_000_000), confirmed(&scene.ours, 2, 80_000)];
        let selection = scene.select(&scene.selection(50_000, ONE_PER_VB), &coins).unwrap();
        assert_eq!(selection.inputs.len(), 1);
        assert!(selection.inputs.iter().all(Utxo::is_confirmed), "{:?}", selection.inputs);

        // Only when the confirmed coins can't pay does the mempool coin come in.
        let selection = scene.select(&scene.selection(500_000, ONE_PER_VB), &coins).unwrap();
        assert!(selection.inputs.iter().any(|coin| !coin.is_confirmed()));
    }

    /// A large mempool coin ordered behind many small confirmed ones would leave
    /// every set that reaches it too heavy to relay.
    #[test]
    fn a_large_mempool_coin_is_not_stuck_behind_many_small_confirmed_ones() {
        let scene = scene(&BITCOIN, ScriptKind::P2pkh).unwrap();
        let mut coins = many_coins(&scene, 700, 1_000);
        coins.push(unconfirmed(&scene.ours, 1, 100_000_000));
        let selection = scene.select(&scene.selection(50_000_000, ONE_PER_VB), &coins).unwrap();
        assert_eq!(selection.inputs.len(), 1, "{:?}", selection.inputs);
        assert!(selection.inputs.iter().all(|coin| !coin.is_confirmed()));
    }

    /// The sender of an unconfirmed payment can still replace it, which would
    /// invalidate a spend of it, so only the wallet's own change is trusted.
    #[test]
    fn someone_elses_unconfirmed_payment_is_never_spent_and_our_own_change_is() {
        let scene = bitcoin_scene().unwrap();
        let request = scene.selection(500_000, ONE_PER_VB);
        let theirs = vec![incoming(&scene.ours, 1, 1_000_000)];
        assert!(matches!(
            scene.select(&request, &theirs),
            Err(UtxoError::InsufficientFunds { .. })
        ));
        let ours = vec![unconfirmed(&scene.ours, 1, 1_000_000)];
        scene.select(&request, &ours).unwrap();
    }

    #[test]
    fn a_refusal_reports_only_what_the_wallet_could_spend() {
        let scene = bitcoin_scene().unwrap();
        let coins = vec![incoming(&scene.ours, 1, 1_000_000), confirmed(&scene.ours, 2, 40_000)];
        match scene.select(&scene.selection(500_000, ONE_PER_VB), &coins) {
            Err(UtxoError::InsufficientFunds { available, .. }) => {
                assert_eq!(available, units(40_000));
            }
            other => panic!("expected a refusal naming the confirmed total, got {other:?}"),
        }
    }

    #[test]
    fn a_refusal_is_short_by_exactly_what_a_send_max_could_not_cover() {
        let scene = bitcoin_scene().unwrap();
        // At ten per vB a P2WPKH input costs 680 base units to spend, so the
        // 600-unit coins are never candidates.
        let mut coins = scene.coins(&[100_000]);
        coins.extend((0..3u8).map(|index| confirmed(&scene.ours, index.saturating_add(20), 600)));

        let target = 200_000u64;
        let refused = scene.select(&scene.selection(target, TEN_PER_VB), &coins);
        let Err(UtxoError::InsufficientFunds { needed, available }) = refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        assert_eq!(available, units(100_000), "the uneconomic coins can pay nothing");
        assert!(needed > available, "{needed} must exceed {available}");

        // A send-max out of the same coins is the most this address can deliver, and
        // the refusal is short by exactly the amount the payment exceeds it.
        let mut drain = scene.selection(0, TEN_PER_VB);
        drain.should_drain = true;
        let sendable = scene.select(&drain, &coins).unwrap().recipient_value;
        assert_eq!(
            needed.checked_sub(available),
            units(target).checked_sub(sendable),
            "needed {needed} against available {available}, send-max {sendable}"
        );
    }

    #[test]
    fn a_refusal_from_an_address_with_nothing_spendable_still_adds_up() {
        let scene = bitcoin_scene().unwrap();
        let refused = scene.select(&scene.selection(50_000, ONE_PER_VB), &[]);
        let Err(UtxoError::InsufficientFunds { needed, available }) = refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        assert_eq!(available, Amount::ZERO);
        assert!(needed > units(50_000), "{needed} must cover the payment and a fee");
    }

    #[test]
    fn an_exact_match_produces_no_change_output() {
        let scene = bitcoin_scene().unwrap();
        let target = 50_000u64;
        // One input and one output are 110 vB.
        let coins = scene.coins(&[target + 110]);
        let selection = scene.select(&scene.selection(target, ONE_PER_VB), &coins).unwrap();
        assert_eq!(selection.strategy, Strategy::Changeless);
        assert_eq!(selection.change, None);
        assert_eq!(selection.recipient_value, units(target));
        assert_eq!(selection.fee, units(110));
        assert_eq!(selection.donated, Amount::ZERO);
    }

    #[test]
    fn a_leftover_below_the_change_floor_is_handed_to_the_miner() {
        let scene = bitcoin_scene().unwrap();
        let floor = change_floor(&scene.params, ONE_PER_VB, scene.ours.script()).unwrap();
        let target = 50_000u64;
        // Enough for the one-output fee plus a leftover one unit under the floor.
        let leftover = u64::try_from(floor.base_units()).unwrap() - 1;
        let coins = scene.coins(&[target + 110 + leftover]);
        let selection = scene.select(&scene.selection(target, ONE_PER_VB), &coins).unwrap();
        assert_eq!(selection.change, None);
        assert_eq!(selection.donated, units(leftover));
        assert_eq!(selection.fee, units(110 + leftover));
    }

    #[test]
    fn a_leftover_at_the_change_floor_comes_back_as_change() {
        let scene = bitcoin_scene().unwrap();
        let floor = change_floor(&scene.params, ONE_PER_VB, scene.ours.script()).unwrap();
        let target = 50_000u64;
        // The two-output form is 141 vB.
        let coins = scene.coins(&[target + 141 + u64::try_from(floor.base_units()).unwrap()]);
        let selection = scene.select(&scene.selection(target, ONE_PER_VB), &coins).unwrap();
        assert_eq!(selection.change, Some(floor));
        assert_eq!(selection.donated, Amount::ZERO);
        assert_eq!(selection.fee, units(141));
    }

    #[test]
    fn litecoin_needs_ten_times_as_much_left_over_before_it_makes_change() {
        let btc = bitcoin_scene().unwrap();
        let ltc = litecoin_scene().unwrap();
        let target = 50_000u64;
        let leftover = 1_000u64;

        let coins = btc.coins(&[target + 141 + leftover]);
        assert_eq!(
            btc.select(&btc.selection(target, ONE_PER_VB), &coins).unwrap().change,
            Some(units(leftover))
        );

        let coins = ltc.coins(&[target + 141 + leftover]);
        let selection = ltc.select(&ltc.selection(target, ONE_PER_VB), &coins).unwrap();
        assert_eq!(selection.change, None);
        assert!(!selection.donated.is_zero());
    }

    #[test]
    fn the_waste_metric_consolidates_only_when_fees_are_low() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[60_000, 40_000, 30_000, 20_000]);

        let mut cheap = scene.selection(50_000, ONE_PER_VB);
        cheap.long_term_fee_rate = FIFTY_PER_VB;
        let consolidating = scene.select(&cheap, &coins).unwrap();

        let mut dear = scene.selection(50_000, FIFTY_PER_VB);
        dear.long_term_fee_rate = ONE_PER_VB;
        let sparing = scene.select(&dear, &coins).unwrap();

        assert!(
            consolidating.inputs.len() > sparing.inputs.len(),
            "{} against {}",
            consolidating.inputs.len(),
            sparing.inputs.len()
        );
    }

    #[test]
    fn a_high_rate_spends_one_large_coin_rather_than_many_small_ones() {
        let scene = bitcoin_scene().unwrap();
        let mut coins = scene.coins(&[200_000]);
        coins
            .extend((0..8u8).map(|index| confirmed(&scene.ours, index.saturating_add(20), 30_000)));
        let mut request = scene.selection(100_000, FIFTY_PER_VB);
        request.long_term_fee_rate = ONE_PER_VB;
        let selection = scene.select(&request, &coins).unwrap();
        assert_eq!(selection.inputs.len(), 1);
        assert_eq!(selection.inputs.first().map(Utxo::value), Some(units(200_000)));
    }

    /// Below the long-term rate every extra input lowers the waste, so consolidation
    /// stops where its fee reaches what the lightest set would cost at the long-term
    /// rate.
    #[test]
    fn a_cheap_rate_never_pays_more_to_consolidate_than_the_long_term_rate_would() {
        let scene = bitcoin_scene().unwrap();
        // The large coin alone pays, and 1,500 small ones could be swept in with it.
        let mut coins = scene.coins(&[1_000_000]);
        coins.extend(many_coins(&scene, 1_500, 10_000));
        let mut cheap = scene.selection(50_000, ONE_PER_VB);
        cheap.long_term_fee_rate = TEN_PER_VB;
        let consolidating = scene.select(&cheap, &coins).unwrap();

        let dear = SelectionRequest { fee_rate: TEN_PER_VB, ..cheap };
        let sparing = scene.select(&dear, &coins).unwrap();
        assert_eq!(sparing.inputs.len(), 1);
        assert!(consolidating.inputs.len() > 1, "a cheap rate still consolidates");
        assert!(
            consolidating.fee <= sparing.fee,
            "{} at one per vB against {} at ten",
            consolidating.fee,
            sparing.fee
        );
    }

    /// Every mempool input lengthens a chain that relay policy limits, so only
    /// confirmed coins are swept in.
    #[test]
    fn a_cheap_rate_never_consolidates_mempool_coins() {
        let scene = bitcoin_scene().unwrap();
        // The confirmed coins can't pay alone, so mempool coins are admitted.
        let mut coins = scene.coins(&[10_000, 10_000, 10_000]);
        coins.push(unconfirmed(&scene.ours, 10, 1_000_000));
        coins.extend((20..60u8).map(|seed| unconfirmed(&scene.ours, seed, 10_000)));
        let mut request = scene.selection(100_000, ONE_PER_VB);
        request.long_term_fee_rate = TEN_PER_VB;

        let selection = scene.select(&request, &coins).unwrap();
        let pending = selection.inputs.iter().filter(|coin| !coin.is_confirmed()).count();
        assert_eq!(pending, 1, "{:?}", selection.inputs);
        assert_eq!(selection.inputs.len(), 4, "every confirmed coin is swept in");
    }

    /// With an odd number of P2TR inputs the search's fee model runs 2 wu over what a
    /// node charges.
    ///
    /// These three coins look changeless to it, and below the long-term rate their
    /// extra inputs make them its first choice, but once priced they leave exactly
    /// the change floor.
    #[test]
    fn a_set_the_model_misjudges_is_passed_over_for_one_that_needs_no_change() {
        let scene = scene(&BITCOIN, ScriptKind::P2tr).unwrap();
        let mut request = scene.selection(10_000, FeeRate::from_sat_per_kwu(1_000));
        request.long_term_fee_rate = TEN_PER_VB;
        let mut coins = scene.coins(&[3_943, 3_943, 3_942]);
        let priced = scene.select(&request, &coins).unwrap();
        assert_eq!(priced.inputs.len(), 3);
        assert!(priced.change.is_some(), "{priced:?}");

        coins.push(confirmed(&scene.ours, 9, 10_946));
        let selection = scene.select(&request, &coins).unwrap();
        assert_eq!(selection.strategy, Strategy::Changeless);
        assert_eq!(
            selection.inputs.iter().map(Utxo::value).collect::<Vec<_>>(),
            vec![units(10_946)]
        );
    }

    #[test]
    fn a_payment_only_an_oversized_set_could_fund_is_refused_for_its_weight() {
        let scene = scene(&BITCOIN, ScriptKind::P2pkh).unwrap();
        // About 675 legacy inputs fit in one transaction, and they hold too little.
        let coins = many_coins(&scene, 700, 1_000);
        let refused = scene.select(&scene.selection(580_000, ONE_PER_VB), &coins);
        let Err(UtxoError::TooHeavy { weight, .. }) = refused else {
            panic!("expected a refusal for weight, got {refused:?}");
        };
        // The refusal names the weight of the fewest coins that pay, which is 681.
        let fewest =
            predict_weight([UtxoScript::P2pkh; 681], [scene.theirs.script().script_pubkey()]);
        assert_eq!(weight, fewest);
    }

    #[test]
    fn draining_pays_the_fee_out_of_the_amount_and_makes_no_change() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[70_000, 30_000]);
        let mut request = scene.selection(0, ONE_PER_VB);
        request.should_drain = true;
        let selection = scene.select(&request, &coins).unwrap();

        assert_eq!(selection.strategy, Strategy::Drain);
        assert_eq!(selection.inputs.len(), 2);
        assert_eq!(selection.change, None);
        assert_eq!(selection.donated, Amount::ZERO);
        assert_eq!(selection.input_total().unwrap(), units(100_000));
        assert_eq!(
            selection.recipient_value.checked_add(selection.fee),
            Some(selection.input_total().unwrap())
        );
    }

    #[test]
    fn draining_spends_every_coin_the_wallet_can_trust() {
        let scene = bitcoin_scene().unwrap();
        let coins = vec![
            confirmed(&scene.ours, 1, 200_000),
            unconfirmed(&scene.ours, 2, 300_000),
            incoming(&scene.ours, 3, 400_000),
        ];
        let mut request = scene.selection(0, ONE_PER_VB);
        request.should_drain = true;

        let selection = scene.select(&request, &coins).unwrap();
        assert_eq!(selection.inputs.len(), 2, "{:?}", selection.inputs);
        assert!(selection.inputs.iter().all(Utxo::is_trusted));
        assert_eq!(selection.input_total().unwrap(), units(500_000));
        assert_eq!(
            selection.recipient_value.checked_add(selection.fee),
            Some(selection.input_total().unwrap())
        );
    }

    #[test]
    fn a_refusal_over_mempool_coins_still_matches_its_send_max() {
        let scene = bitcoin_scene().unwrap();
        let coins = vec![confirmed(&scene.ours, 1, 40_000), unconfirmed(&scene.ours, 2, 60_000)];
        let target = 500_000u64;

        let refused = scene.select(&scene.selection(target, ONE_PER_VB), &coins);
        let Err(UtxoError::InsufficientFunds { needed, available }) = refused else {
            panic!("expected a refusal, got {refused:?}");
        };
        assert_eq!(available, units(100_000), "both tiers failed, so both coins were candidates");

        let mut drain = scene.selection(0, ONE_PER_VB);
        drain.should_drain = true;
        let sendable = scene.select(&drain, &coins).unwrap().recipient_value;
        assert_eq!(
            needed.checked_sub(available),
            units(target).checked_sub(sendable),
            "needed {needed} against available {available}, send-max {sendable}"
        );
    }

    #[test]
    fn draining_an_address_that_cannot_pay_its_own_fee_fails_cleanly() {
        let scene = bitcoin_scene().unwrap();
        let mut request = scene.selection(0, TEN_PER_VB);
        request.should_drain = true;
        assert!(matches!(scene.select(&request, &[]), Err(UtxoError::InsufficientFunds { .. })));
    }

    #[test]
    fn draining_into_a_dust_payment_is_refused_rather_than_built() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[400]);
        let mut request = scene.selection(0, ONE_PER_VB);
        request.should_drain = true;
        assert!(matches!(scene.select(&request, &coins), Err(UtxoError::Dust { .. })));
    }

    #[test]
    fn a_target_above_the_chains_supply_is_refused_before_anything_is_priced() {
        let scene = bitcoin_scene().unwrap();
        let request = SelectionRequest { target: Amount::MAX, ..scene.selection(1, ONE_PER_VB) };
        assert!(matches!(scene.select(&request, &[]), Err(UtxoError::AboveMaxMoney { .. })));
    }

    #[test]
    fn every_strategy_has_a_wire_spelling_without_spaces() {
        for strategy in Strategy::ALL {
            assert!(!strategy.as_str().contains(' '), "{strategy} is a wire spelling");
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 256,
            rng_seed: RngSeed::Fixed(PROPERTY_SEED),
            failure_persistence: None,
            ..ProptestConfig::default()
        })]

        #[test]
        fn a_selection_that_succeeds_always_conserves_value(
            values in prop::collection::vec(1_000u64..2_000_000, 1..7),
            target in 1_000u64..1_000_000,
            rate in 250u64..25_000,
        ) {
            let scene = bitcoin_scene().expect("a scene");
            let coins = scene.coins(&values);
            let request = scene.selection(target, FeeRate::from_sat_per_kwu(rate));
            match scene.select(&request, &coins) {
                Ok(selection) => {
                    let inputs = selection.input_total().expect("a total");
                    let outputs = selection.output_total().expect("a total");
                    prop_assert_eq!(inputs, outputs.checked_add(selection.fee).expect("a total"));
                    prop_assert!(selection.recipient_value >= units(target));
                    prop_assert!(!selection.inputs.is_empty());
                    if let Some(change) = selection.change {
                        prop_assert!(selection.donated.is_zero());
                        let floor = change_floor(
                            &scene.params,
                            request.long_term_fee_rate,
                            request.change_script,
                        ).expect("a floor");
                        prop_assert!(change >= floor);
                    }
                }
                // A refusal never names a total the address already holds, since
                // both figures come from the same coins.
                Err(UtxoError::InsufficientFunds { needed, available }) => {
                    prop_assert!(needed > available, "{} against {}", needed, available);
                }
                Err(_) => {}
            }
        }

        /// Settles every subset of a small pool within the consolidation cap the way a
        /// selection would, and checks the search against the best of them. Repeated
        /// values exercise the skip over identical coins.
        #[test]
        fn the_changeless_search_finds_a_set_whenever_a_brute_force_does(
            values in prop::collection::vec(
                prop::sample::select(vec![3_000u64, 5_000, 8_000, 13_000]),
                1..10,
            ),
            target in 1_000u64..40_000,
            rate in prop::sample::select(vec![250u64, 1_000, 2_600, 7_777]),
            long_term in prop::sample::select(vec![250u64, 2_500]),
        ) {
            let scene = bitcoin_scene().expect("a scene");
            let coins = scene.coins(&values);
            let mut request = scene.selection(target, FeeRate::from_sat_per_kwu(rate));
            request.long_term_fee_rate = FeeRate::from_sat_per_kwu(long_term);
            let floor = change_floor(
                &scene.params,
                request.long_term_fee_rate,
                request.change_script,
            ).expect("a floor");
            let per_input = i128::from(rate)
                .checked_sub(i128::from(long_term))
                .and_then(|step| step.checked_mul(i128::from(input_weight(UtxoScript::P2wpkh).to_wu())))
                .expect("a small timing cost");
            // The waste metric in thousandths of a base unit, as the selection scores it.
            let waste = |count: usize, donated: Amount| -> Option<i128> {
                let excess = i128::try_from(donated.base_units()).ok()?.checked_mul(1000)?;
                i128::try_from(count).ok()?.checked_mul(per_input)?.checked_add(excess)
            };

            // The weight a set is priced at, and what it donates when it makes no change.
            // `None` when it can't pay.
            let settle = |picked: &[Utxo]| -> Option<(Weight, Option<Amount>)> {
                let kinds = vec![UtxoScript::P2wpkh; picked.len()];
                let plain = predict_weight(kinds.clone(), [request.recipient_script_pubkey]);
                let with_change = predict_weight(kinds, [request.recipient_script_pubkey, request.change_script.script_pubkey()]);
                let spare = Utxo::total(picked).ok()?.checked_sub(units(target))?;
                let fee_with_change = fee_for(request.fee_rate, with_change).ok()?;
                if spare.checked_sub(fee_with_change).is_some_and(|change| change >= floor) {
                    return Some((with_change, None));
                }
                let donated = spare.checked_sub(fee_for(request.fee_rate, plain).ok()?)?;
                Some((plain, Some(donated)))
            };
            // Below the long-term rate no set may outweigh the fewest largest coins that
            // pay by more than the ratio of the rates.
            let mut largest_first = coins.clone();
            largest_first.sort_by_key(|coin| Reverse(coin.value()));
            let lightest = (1..=largest_first.len())
                .find_map(|count| settle(largest_first.get(..count)?))
                .map(|(weight, _)| weight.to_wu());
            let cap_wu = match lightest {
                Some(lightest) if rate < long_term => lightest
                    .checked_mul(long_term)
                    .and_then(|scaled| scaled.checked_div(rate))
                    .expect("a small cap"),
                _ => u64::MAX,
            };

            let mut least: Option<i128> = None;
            for mask in 1u32..(1 << coins.len()) {
                let picked: Vec<Utxo> = coins
                    .iter()
                    .enumerate()
                    .filter(|(index, _)| mask & (1 << index) != 0)
                    .map(|(_, coin)| coin.clone())
                    .collect();
                if let Some((weight, Some(donated))) = settle(&picked)
                    && weight.to_wu() <= cap_wu
                {
                    let wasted = waste(picked.len(), donated).expect("a small waste");
                    least = Some(least.map_or(wasted, |found| found.min(wasted)));
                }
            }

            let selected = scene.select(&request, &coins);
            if let Some(least) = least {
                let selection = selected.expect("a changeless set exists");
                prop_assert_eq!(selection.strategy, Strategy::Changeless);
                // Below the long-term rate a larger set in the window can waste less, and
                // the search, like Core's, never grows a set that is already in it.
                if rate >= long_term {
                    let wasted =
                        waste(selection.inputs.len(), selection.donated).expect("a small waste");
                    // The search models fees to the thousandth of a base unit and a node
                    // rounds them up, so it can miss the least waste by under one unit.
                    prop_assert!(
                        wasted < least.saturating_add(1000),
                        "{} against {}",
                        wasted,
                        least
                    );
                }
            } else {
                let strategy = selected.ok().map(|selection| selection.strategy);
                prop_assert_ne!(strategy, Some(Strategy::Changeless));
            }
        }
    }
}
