use std::iter::once;

use bitcoin::{Script, Transaction, TxOut, Weight};
use jelly_core::{Amount, FeeRate};

use crate::build::{BuiltTx, Source, assemble, participants};
use crate::coin::{Utxo, tx_out, value_of};
use crate::error::UtxoError;
use crate::original::Original;
use crate::params::UtxoParams;
use crate::payload::{Intent, PaymentReduction, Replaced, UtxoPayload};
use crate::script::SpendableScript;
use crate::select::{Eligible, Selection, Strategy, ordered_candidates};
use crate::{fee, weight};

#[derive(Debug, Clone)]
pub(crate) struct ReplacementRequest<'a> {
    pub(crate) fee_rate: FeeRate,
    /// The addresses the replacement may spend from, the change address first.
    pub(crate) sources: Vec<Source<'a>>,
    /// The address the payload names as the recipient.
    pub(crate) to: &'a str,
    /// The script the payment output pays, token prefix and all. Supplied by the
    /// caller, so the planner itself never guesses which output was the payment.
    pub(crate) to_script: &'a Script,
    /// Coins of the sources the planner may add. BIP-125 rule 2 forbids adding an
    /// unconfirmed one, so those are dropped, along with coins that carry tokens and
    /// coinbase or peg-out outputs not yet mature at `tip_height`.
    pub(crate) extras: &'a [Utxo],
    pub(crate) tip_height: u32,
    /// The total fee of the original's mempool descendants, which the replacement
    /// evicts too.
    pub(crate) descendant_fee: Amount,
}

/// Where the payment and the change sit in the original.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    recipient: usize,
    change: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlannedReplacement {
    /// The original's inputs first, in their original order, then any added.
    inputs: Vec<Utxo>,
    /// In the original's order, minus change that couldn't survive, plus any new
    /// change at the end.
    outputs: Vec<TxOut>,
    priced: Priced,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Priced {
    /// What the recipient ends up with.
    payment: Amount,
    change: Option<Amount>,
    fee: Amount,
    weight: Weight,
    /// Value above the required fee, which goes to the miner because change that
    /// small is worth less than it costs. Included in `fee`.
    donated: Amount,
    /// Set when the payment itself had to shrink.
    reduced_payment: Option<PaymentReduction>,
}

/// Plans a BIP-125 replacement at `request.fee_rate`.
///
/// The extra fee comes from shrinking change, then dropping it, then adding
/// confirmed inputs, and last from shrinking the payment. Uneconomic change goes
/// to the miner and produces a preview warning.
fn plan_replacement(
    params: &UtxoParams,
    original: &Original,
    request: &ReplacementRequest<'_>,
) -> Result<PlannedReplacement, UtxoError> {
    if !original.is_replaceable() {
        return Err(UtxoError::NotReplaceable { txid: original.txid() });
    }
    if request.fee_rate <= original.rate() {
        return Err(UtxoError::RateNotIncreased {
            txid: original.txid(),
            old: original.rate(),
            unit: params.fee_rate_unit,
        });
    }

    let own =
        request.sources.first().ok_or(UtxoError::payload("a replacement names no source"))?.script;
    let layout = layout_of(original.tx(), request.to_script, own.script_pubkey())?;
    let payment = original.tx().output.get(layout.recipient).ok_or(UtxoError::RecipientNotFound)?;
    let fixed = fixed_outputs(original.tx(), layout);
    let fixed_total = Amount::checked_sum(fixed.iter().map(|&(_, txout)| value_of(txout)))
        .ok_or_else(|| UtxoError::overflow("a fixed output total"))?;

    let already = original.spends();
    let extras: Vec<Utxo> =
        ordered_candidates(request.extras, request.fee_rate, Eligible::Confirmed)
            .into_iter()
            .filter(|coin| {
                !already.contains(&coin.outpoint())
                    && coin.is_selectable(params, request.tip_height)
            })
            .collect();

    let planner = Planner {
        params,
        request,
        original,
        evicted_fee: original.evicted_fee(request.descendant_fee)?,
        payment,
        fixed: &fixed,
        fixed_total,
        own,
        layout,
        change_floor: fee::change_floor(params, params.long_term_fee_rate, own)?,
        recipient_dust: params.dust_threshold(payment.script_pubkey.as_script()),
    };

    planner.fund(extras)
}

pub(crate) fn build_replacement(
    params: &UtxoParams,
    original: &Original,
    request: &ReplacementRequest<'_>,
) -> Result<BuiltTx, UtxoError> {
    let PlannedReplacement { inputs, outputs, priced } =
        plan_replacement(params, original, request)?;
    weight::check_standard_weight(params, priced.weight)?;
    fee::check_fee_ceiling(params, priced.fee)?;

    let tx = assemble(params.replacement.input_sequence(), &inputs, outputs, request.tip_height)?;
    let payload = UtxoPayload::assemble(
        tx,
        &inputs,
        &Intent {
            sources: participants(&request.sources, &inputs, priced.change.is_some()),
            to: request.to.to_owned(),
            strategy: Strategy::Replacement,
            donated: priced.donated,
            replaces: Some(Replaced {
                txid: original.txid(),
                reduced_payment: priced.reduced_payment,
            }),
            pays_for: None,
        },
    )?;
    let selection = Selection {
        inputs,
        recipient_value: priced.payment,
        change: priced.change,
        donated: priced.donated,
        fee: priced.fee,
        weight: priced.weight,
        strategy: Strategy::Replacement,
    };
    Ok(BuiltTx { payload, selection })
}

/// The lowest rate the network will accept as a replacement for `original`,
/// assuming the replacement is the same size, when its descendants pay
/// `descendant_fee`.
///
/// Derived from the absolute floor with [`fee::rate_that_pays`], because a rate
/// from a rounded-down division wouldn't pay that fee back.
pub(crate) fn min_replacement_rate(
    params: &UtxoParams,
    original: &Original,
    descendant_fee: Amount,
) -> Result<FeeRate, UtxoError> {
    let evicted = original.evicted_fee(descendant_fee)?;
    let floor = fee::replacement_floor(params, evicted, original.weight())?;
    let by_floor = fee::rate_that_pays(floor, original.weight())?;
    let above_old = FeeRate::from_sat_per_kwu(original.rate().sat_per_kwu().saturating_add(1));
    Ok(by_floor.max(above_old))
}

/// The payment is the first output paying the recorded recipient, and the change
/// is the last other output paying the change address.
fn layout_of(
    tx: &Transaction,
    recipient_script_pubkey: &Script,
    own_script_pubkey: &Script,
) -> Result<Layout, UtxoError> {
    let recipient = tx
        .output
        .iter()
        .position(|txout| txout.script_pubkey.as_script() == recipient_script_pubkey)
        .ok_or(UtxoError::RecipientNotFound)?;
    let change = tx
        .output
        .iter()
        .enumerate()
        .rev()
        .find(|(index, txout)| {
            *index != recipient && txout.script_pubkey.as_script() == own_script_pubkey
        })
        .map(|(index, _)| index);
    Ok(Layout { recipient, change })
}

/// The outputs that are neither the payment nor the change. Jelly never builds one,
/// but a transaction from elsewhere might.
fn fixed_outputs(tx: &Transaction, layout: Layout) -> Vec<(usize, &TxOut)> {
    tx.output
        .iter()
        .enumerate()
        .filter(|&(index, _)| index != layout.recipient && Some(index) != layout.change)
        .collect()
}

/// What stays fixed while `fund` tries input sets.
struct Planner<'a> {
    params: &'a UtxoParams,
    request: &'a ReplacementRequest<'a>,
    original: &'a Original,
    evicted_fee: Amount,
    payment: &'a TxOut,
    fixed: &'a [(usize, &'a TxOut)],
    fixed_total: Amount,
    own: &'a SpendableScript,
    layout: Layout,
    change_floor: Amount,
    recipient_dust: Amount,
}

impl Planner<'_> {
    /// Adds confirmed inputs one at a time until the higher fee fits, keeping the
    /// change output while it's worth keeping and folding it into the fee once it
    /// isn't.
    fn fund(&self, extras: Vec<Utxo>) -> Result<PlannedReplacement, UtxoError> {
        let payment_value = value_of(self.payment);
        let mut inputs = self.original.inputs().to_vec();
        let mut extras = extras.into_iter();
        loop {
            let available = Utxo::total(&inputs)?;
            if let Some(spendable) = available
                .checked_sub(self.fixed_total)
                .and_then(|left| left.checked_sub(payment_value))
            {
                let (weight, required) = self.requirement(&inputs, true)?;
                if let Some(change) = spendable.checked_sub(required)
                    && change >= self.change_floor
                {
                    return self.finish(
                        inputs,
                        Priced {
                            payment: payment_value,
                            change: Some(change),
                            fee: required,
                            weight,
                            donated: Amount::ZERO,
                            reduced_payment: None,
                        },
                    );
                }

                let (weight, required) = self.requirement(&inputs, false)?;
                if let Some(donated) = spendable.checked_sub(required) {
                    return self.finish(
                        inputs,
                        Priced {
                            payment: payment_value,
                            change: None,
                            fee: spendable,
                            weight,
                            donated,
                            reduced_payment: None,
                        },
                    );
                }
            }
            let Some(coin) = extras.next() else {
                return self.reduce(inputs);
            };
            inputs.push(coin);
        }
    }

    /// Takes the extra fee out of the payment, as a last resort. This is the only
    /// way to unstick a send-max, which has no change and no coins left over.
    fn reduce(&self, inputs: Vec<Utxo>) -> Result<PlannedReplacement, UtxoError> {
        let payment_value = value_of(self.payment);
        let available = Utxo::total(&inputs)?;
        let (weight, required) = self.requirement(&inputs, false)?;
        let reduced = available
            .checked_sub(self.fixed_total)
            .and_then(|left| left.checked_sub(required))
            .unwrap_or(Amount::ZERO);
        if reduced >= self.recipient_dust {
            return self.finish(
                inputs,
                Priced {
                    payment: reduced,
                    change: None,
                    fee: required,
                    weight,
                    donated: Amount::ZERO,
                    reduced_payment: Some(PaymentReduction { from: payment_value, to: reduced }),
                },
            );
        }
        Err(UtxoError::InsufficientFunds {
            needed: self.fixed_total.saturating_add(payment_value).saturating_add(required),
            available,
        })
    }

    /// The weight and fee floor of a replacement spending `inputs`, with or
    /// without a change output.
    fn requirement(
        &self,
        inputs: &[Utxo],
        has_change: bool,
    ) -> Result<(Weight, Amount), UtxoError> {
        let scripts = once(self.payment.script_pubkey.as_script())
            .chain(self.fixed.iter().map(|&(_, txout)| txout.script_pubkey.as_script()))
            .chain(has_change.then(|| self.own.script_pubkey()));
        let weight = weight::predict_weight(inputs.iter().map(Utxo::kind), scripts);
        let by_rate = fee::fee_for(self.request.fee_rate, weight)?;
        let by_rule_four = fee::replacement_floor(self.params, self.evicted_fee, weight)?;
        Ok((weight, by_rate.max(by_rule_four)))
    }

    /// Rebuilds the output list in the original's order.
    fn finish(&self, inputs: Vec<Utxo>, priced: Priced) -> Result<PlannedReplacement, UtxoError> {
        let count = self.original.tx().output.len();
        let mut outputs: Vec<TxOut> = Vec::with_capacity(count.saturating_add(1));
        for index in 0..count {
            if index == self.layout.recipient {
                outputs.push(tx_out(priced.payment, self.payment.script_pubkey.clone())?);
            } else if Some(index) == self.layout.change {
                if let Some(value) = priced.change {
                    outputs.push(tx_out(value, self.own.script_buf().clone())?);
                }
            } else if let Some(&(_, txout)) =
                self.fixed.iter().find(|&&(position, _)| position == index)
            {
                outputs.push(txout.clone());
            }
        }
        // A replacement can add change the original didn't have, so a large leftover
        // doesn't go to the miner.
        if self.layout.change.is_none()
            && let Some(value) = priced.change
        {
            outputs.push(tx_out(value, self.own.script_buf().clone())?);
        }
        Ok(PlannedReplacement { inputs, outputs, priced })
    }
}

#[cfg(test)]
mod tests {
    use bitcoin::Sequence;
    use jelly_core::{ScriptKind, TxWarningKind};

    use super::*;
    use crate::coin::units;
    use crate::fee::{fee_for, rate_that_pays, replacement_floor};
    use crate::fixtures::{
        FLOOR_QUOTE, Fallible, ONE_PER_VB, Scene, TEN_PER_VB, TIP, bitcoin_scene, confirmed, kwu,
        sent_original, unconfirmed,
    };
    use crate::preview::preview;
    use crate::script::UtxoScript;
    use crate::sign::sign_tx;
    use crate::test_util::test_key;
    use crate::weight::predict_weight;

    const FUNDED_SAT: u64 = 500_000;
    const PAID_SAT: u64 = 100_000;

    /// A signed Bitcoin transaction this crate built, with the transaction that funded
    /// it.
    struct Story {
        scene: Scene,
        original: Original,
        funding: Vec<Transaction>,
    }

    fn story(amount: Option<u64>, rate: FeeRate, funded: u64) -> Fallible<Story> {
        let scene = bitcoin_scene()?;
        let (original, funding) = sent_original(&scene, amount, rate, funded)?;
        Ok(Story { scene, original, funding })
    }

    impl Story {
        fn request<'a>(&'a self, rate: FeeRate, extras: &'a [Utxo]) -> ReplacementRequest<'a> {
            ReplacementRequest {
                fee_rate: rate,
                sources: self.scene.own_source().to_vec(),
                to: self.scene.theirs.address(),
                to_script: self.scene.theirs.script().script_pubkey(),
                extras,
                tip_height: TIP,
                descendant_fee: Amount::ZERO,
            }
        }

        fn plan(&self, rate: FeeRate) -> Result<PlannedReplacement, UtxoError> {
            plan_replacement(&self.scene.params, &self.original, &self.request(rate, &[]))
        }
    }

    #[test]
    fn the_change_output_pays_for_the_bump() {
        let story = story(Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        let before = story.original.fee();
        let plan = story.plan(TEN_PER_VB).unwrap();

        assert_eq!(plan.inputs.len(), story.original.inputs().len());
        assert_eq!(plan.inputs.len(), 1);
        assert_eq!(
            plan.priced.payment,
            units(PAID_SAT),
            "the recipient still gets what was promised"
        );
        assert!(plan.priced.change.is_some());
        assert!(plan.priced.fee > before, "{} must beat {before}", plan.priced.fee);
        assert_eq!(plan.priced.reduced_payment, None);
        assert_eq!(plan.priced.donated, Amount::ZERO);
        let layout = layout_of(
            story.original.tx(),
            story.scene.theirs.script().script_pubkey(),
            story.scene.ours.script().script_pubkey(),
        )
        .unwrap();
        assert_eq!(layout, Layout { recipient: 0, change: Some(1) });
    }

    /// BIP-125 rule 4 is a floor on the absolute fee, so when the requested rate
    /// doesn't clear it the floor wins.
    #[test]
    fn the_incremental_relay_floor_beats_the_requested_rate_when_it_has_to() {
        let story = story(Some(PAID_SAT), TEN_PER_VB, FUNDED_SAT).unwrap();
        // One unit per kilo-weight-unit above the old rate is a rate rise and nowhere
        // near the absolute floor.
        let barely = kwu(story.original.rate().sat_per_kwu().saturating_add(1));
        let plan = story.plan(barely).unwrap();
        let floor =
            replacement_floor(&story.scene.params, story.original.fee(), plan.priced.weight)
                .unwrap();
        assert!(plan.priced.fee >= floor, "{} must reach {floor}", plan.priced.fee);
        assert!(plan.priced.fee > fee_for(barely, plan.priced.weight).unwrap());
    }

    #[test]
    fn a_transaction_that_did_not_signal_replaceability_cannot_be_replaced() {
        let story = story(Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        let mut tx = story.original.tx().clone();
        for input in &mut tx.input {
            input.sequence = Sequence::MAX;
        }
        let final_original = Original::new(tx, &story.funding).unwrap();
        assert!(!final_original.is_replaceable());
        assert!(matches!(
            plan_replacement(&story.scene.params, &final_original, &story.request(TEN_PER_VB, &[])),
            Err(UtxoError::NotReplaceable { .. })
        ));
    }

    #[test]
    fn a_replacement_that_does_not_raise_the_rate_is_refused() {
        let story = story(Some(PAID_SAT), TEN_PER_VB, FUNDED_SAT).unwrap();
        let old = story.original.rate();
        assert!(matches!(story.plan(old), Err(UtxoError::RateNotIncreased { .. })));
        assert!(matches!(story.plan(ONE_PER_VB), Err(UtxoError::RateNotIncreased { .. })));
        story.plan(kwu(old.sat_per_kwu().saturating_add(1))).unwrap();
    }

    #[test]
    fn change_that_is_no_longer_worth_keeping_goes_to_the_miner() {
        // Fund the original so tightly that raising the fee eats the change.
        let story = story(Some(PAID_SAT), ONE_PER_VB, PAID_SAT + 500).unwrap();
        let plan =
            story.plan(kwu(story.original.rate().sat_per_kwu().saturating_add(300))).unwrap();
        assert_eq!(plan.priced.change, None, "the change could not survive");
        assert_eq!(plan.outputs.len(), 1);
        assert_eq!(plan.priced.payment, units(PAID_SAT), "the recipient is untouched");
        assert!(!plan.priced.donated.is_zero(), "the leftover is recorded");
    }

    #[test]
    fn extra_confirmed_inputs_are_added_when_the_outputs_cannot_pay() {
        let story = story(None, ONE_PER_VB, FUNDED_SAT).unwrap();
        // A send-max has no change, so a much higher rate needs another coin.
        let extras = vec![confirmed(&story.scene.ours, 5, 200_000)];
        let plan = plan_replacement(
            &story.scene.params,
            &story.original,
            &story.request(TEN_PER_VB, &extras),
        )
        .unwrap();
        assert_eq!(plan.inputs.len(), story.original.inputs().len() + 1);
        assert_eq!(plan.inputs.len(), 2);
        assert_eq!(
            plan.priced.payment,
            units(FUNDED_SAT).checked_sub(story.original.fee()).expect("a payment")
        );
    }

    #[test]
    fn extra_inputs_stop_at_the_first_set_that_preserves_the_payment() {
        let story = story(None, ONE_PER_VB, FUNDED_SAT).unwrap();
        let extras = vec![
            confirmed(&story.scene.ours, 5, 1_300),
            confirmed(&story.scene.ours, 6, 1_500),
            confirmed(&story.scene.ours, 7, 1_400),
        ];
        let plan = plan_replacement(
            &story.scene.params,
            &story.original,
            &story.request(TEN_PER_VB, &extras),
        )
        .unwrap();
        let outpoints: Vec<_> = plan.inputs.iter().map(Utxo::outpoint).collect();
        let mut expected = story.original.spends();
        expected.extend([extras[1].outpoint(), extras[2].outpoint()]);
        assert_eq!(outpoints, expected);
        assert_eq!(plan.priced.payment, value_of(&story.original.tx().output[0]));
        assert_eq!(plan.priced.reduced_payment, None);
    }

    #[test]
    fn payment_reduction_keeps_every_eligible_extra_input() {
        let story = story(None, ONE_PER_VB, FUNDED_SAT).unwrap();
        let extras = vec![
            confirmed(&story.scene.ours, 5, 800),
            confirmed(&story.scene.ours, 6, 1_000),
            confirmed(&story.scene.ours, 7, 900),
        ];
        let plan = plan_replacement(
            &story.scene.params,
            &story.original,
            &story.request(TEN_PER_VB, &extras),
        )
        .unwrap();
        let mut expected = story.original.spends();
        expected.extend([extras[1].outpoint(), extras[2].outpoint(), extras[0].outpoint()]);
        assert_eq!(plan.inputs.iter().map(Utxo::outpoint).collect::<Vec<_>>(), expected);
        assert!(plan.priced.reduced_payment.is_some());
        assert_eq!(
            Utxo::total(&plan.inputs).unwrap(),
            plan.priced.payment.checked_add(plan.priced.fee).unwrap()
        );
    }

    /// BIP-125 rule 2 forbids a replacement from adding an unconfirmed input.
    #[test]
    fn an_unconfirmed_coin_is_never_added_to_a_replacement() {
        let story = story(None, ONE_PER_VB, FUNDED_SAT).unwrap();
        let extras = vec![unconfirmed(&story.scene.ours, 5, 200_000)];
        let plan = plan_replacement(
            &story.scene.params,
            &story.original,
            &story.request(TEN_PER_VB, &extras),
        )
        .unwrap();
        assert_eq!(plan.inputs.len(), story.original.inputs().len());
        assert!(plan.priced.reduced_payment.is_some(), "so the payment had to shrink instead");
    }

    #[test]
    fn the_payment_shrinks_when_there_is_nothing_else_left() {
        let story = story(None, ONE_PER_VB, FUNDED_SAT).unwrap();
        let before = story.original.tx().output.first().map(|output| output.value.to_sat());
        let plan = story.plan(TEN_PER_VB).unwrap();

        let reduction = plan.priced.reduced_payment.expect("a recorded reduction");
        assert_eq!(reduction.from.base_units(), u128::from(before.expect("a payment")));
        assert_eq!(reduction.to, plan.priced.payment);
        assert!(reduction.to < reduction.from);
        assert_eq!(plan.priced.change, None);
        assert_eq!(plan.inputs.len(), story.original.inputs().len());
    }

    #[test]
    fn a_bump_that_would_dust_the_payment_is_refused() {
        let story = story(None, ONE_PER_VB, FUNDED_SAT).unwrap();
        let recipient = story.scene.theirs.script().script_pubkey();
        let weight = predict_weight([UtxoScript::P2wpkh], [recipient]);
        let dust = story.scene.params.dust_threshold(recipient);
        let leaving = |payment: Amount| {
            let fee = units(FUNDED_SAT).checked_sub(payment).expect("a fee");
            rate_that_pays(fee, weight).expect("a rate")
        };

        let below = dust.checked_sub(units(1)).expect("a payment");
        assert!(matches!(story.plan(leaving(below)), Err(UtxoError::InsufficientFunds { .. })));
        assert_eq!(story.plan(leaving(dust)).unwrap().priced.payment, dust);
    }

    #[test]
    fn a_transaction_with_no_output_for_the_recorded_recipient_is_refused() {
        let story = story(Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        let stranger = test_key(&story.scene.params, ScriptKind::P2wpkh, 9).unwrap();
        let request = ReplacementRequest {
            to: stranger.address(),
            to_script: stranger.script().script_pubkey(),
            ..story.request(TEN_PER_VB, &[])
        };
        assert!(matches!(
            plan_replacement(&story.scene.params, &story.original, &request),
            Err(UtxoError::RecipientNotFound)
        ));
    }

    #[test]
    fn planning_is_deterministic() {
        let story = story(Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        assert_eq!(story.plan(TEN_PER_VB).unwrap(), story.plan(TEN_PER_VB).unwrap());
    }

    #[test]
    fn the_lowest_rate_the_network_will_take_clears_rule_four() {
        let story = story(Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        let minimum =
            min_replacement_rate(&story.scene.params, &story.original, Amount::ZERO).unwrap();
        assert!(
            minimum > story.original.rate(),
            "{minimum:?} must beat {:?}",
            story.original.rate()
        );

        let floor =
            replacement_floor(&story.scene.params, story.original.fee(), story.original.weight())
                .unwrap();
        assert!(fee_for(minimum, story.original.weight()).unwrap() >= floor);
        assert!(story.plan(minimum).is_ok(), "the minimum has to be plannable");

        // The server's own increment raises it.
        let charged = story.scene.params.with_incremental_relay_fee(Some(kwu(2_500)));
        let raised = min_replacement_rate(&charged, &story.original, Amount::ZERO).unwrap();
        assert!(raised > minimum, "{raised:?} must exceed {minimum:?}");
    }

    /// BIP-125 rule 3 counts every transaction a replacement evicts, the original's
    /// descendants as well as the original.
    #[test]
    fn what_the_descendants_paid_raises_the_floor() {
        let story = story(Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        let descendant_fee = units(50_000);
        let request = ReplacementRequest { descendant_fee, ..story.request(TEN_PER_VB, &[]) };
        let plan = plan_replacement(&story.scene.params, &story.original, &request).unwrap();
        let evicted = story.original.fee().checked_add(descendant_fee).expect("a total");
        let floor = replacement_floor(&story.scene.params, evicted, plan.priced.weight).unwrap();
        assert!(plan.priced.fee >= floor, "{} must reach {floor}", plan.priced.fee);

        let alone =
            min_replacement_rate(&story.scene.params, &story.original, Amount::ZERO).unwrap();
        let evicting =
            min_replacement_rate(&story.scene.params, &story.original, descendant_fee).unwrap();
        assert!(evicting > alone, "{evicting:?} must exceed {alone:?}");
    }

    #[test]
    fn a_replacement_becomes_a_signable_payload_that_names_what_it_replaces() {
        let story = story(Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        let built = build_replacement(
            &story.scene.params,
            &story.original,
            &story.request(TEN_PER_VB, &[]),
        )
        .unwrap();

        assert_eq!(built.selection.strategy, Strategy::Replacement);
        let replaced = built.payload.intent().replaces.expect("a recorded original");
        assert_eq!(replaced.txid, story.original.txid());
        assert_eq!(
            built.payload.outpoints(),
            story.original.spends(),
            "a replacement spends what the original spent"
        );

        let rendered = preview(&story.scene.params, &built.payload, FLOOR_QUOTE).unwrap();
        assert!(
            rendered.warnings.iter().any(|warning| warning.kind == TxWarningKind::Replacement),
            "{:?}",
            rendered.warnings
        );
        assert!(rendered.details.iter().any(|detail| detail.label == "Replaces"));

        let signed =
            sign_tx(&story.scene.params, &built.payload, &[story.scene.ours.material()]).unwrap();
        assert_eq!(signed.txid(), built.payload.unsigned_txid());
    }

    #[test]
    fn a_replacement_above_the_fee_ceiling_is_refused() {
        let story = story(Some(PAID_SAT), ONE_PER_VB, FUNDED_SAT).unwrap();
        let capped = UtxoParams { fee_ceiling: units(1), ..story.scene.params };
        assert!(matches!(
            build_replacement(&capped, &story.original, &story.request(TEN_PER_VB, &[])),
            Err(UtxoError::FeeCeiling { .. })
        ));
    }
}
