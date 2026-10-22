//! Child-pays-for-parent, the fee bump of a chain whose nodes relay no replacement.
//!
//! The package arithmetic is Electrum's `get_child_fee_from_total_feerate`: the child
//! pays what brings the package and itself together to the chosen rate, less what the
//! package already paid. Electrum's package is the parent alone. Here it also holds
//! every unconfirmed ancestor the parent waits on, because a miner scores the child
//! with all of them.

use std::iter::once;

use bitcoin::{Txid, Weight};
use jelly_core::{Amount, FeeRate};

use crate::build::{BuiltTx, Source, assemble, participants};
use crate::coin::{Utxo, tx_out};
use crate::error::UtxoError;
use crate::original::Original;
use crate::params::UtxoParams;
use crate::payload::{Intent, UtxoPayload};
use crate::select::{Eligible, Selection, Strategy, ordered_candidates};
use crate::{fee, weight};

/// An unconfirmed transaction a child pays for, as the mempool prices it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unconfirmed {
    pub(crate) txid: Txid,
    pub(crate) fee: Amount,
    pub(crate) weight: Weight,
}

impl From<&Original> for Unconfirmed {
    fn from(original: &Original) -> Self {
        Self { txid: original.txid(), fee: original.fee(), weight: original.weight() }
    }
}

/// The parent a child pays for, and every unconfirmed ancestor the parent waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Package {
    parent: Unconfirmed,
    ancestors: Vec<Unconfirmed>,
}

impl Package {
    pub(crate) const fn new(parent: Unconfirmed, ancestors: Vec<Unconfirmed>) -> Self {
        Self { parent, ancestors }
    }

    pub(crate) const fn parent(&self) -> Txid {
        self.parent.txid
    }

    fn members(&self) -> impl Iterator<Item = &Unconfirmed> {
        once(&self.parent).chain(&self.ancestors)
    }

    fn fee(&self) -> Result<Amount, UtxoError> {
        Amount::checked_sum(self.members().map(|member| member.fee))
            .ok_or_else(|| UtxoError::overflow("a package fee"))
    }

    /// Virtual bytes, counted per transaction as a node counts an ancestor package.
    fn vsize(&self) -> u64 {
        self.members().fold(0, |total, member| total.saturating_add(weight::vsize(member.weight)))
    }

    /// The rate the package pays now, which a child has to raise.
    pub(crate) fn rate(&self) -> Result<FeeRate, UtxoError> {
        let weight =
            Weight::from_vb(self.vsize()).ok_or_else(|| UtxoError::overflow("a package size"))?;
        fee::rate_for(self.fee()?, weight)
    }
}

/// What a child of `child_weight` pays so that it and `package` together pay `rate`,
/// and never less than it needs to be relayed on its own.
fn child_fee(
    package: &Package,
    rate: FeeRate,
    relay_floor: FeeRate,
    child_weight: Weight,
) -> Result<Amount, UtxoError> {
    let total_vsize = package.vsize().saturating_add(weight::vsize(child_weight));
    let total = fee::fee_for(
        rate,
        Weight::from_vb(total_vsize).ok_or_else(|| UtxoError::overflow("a package size"))?,
    )?;
    let lift = total.saturating_sub(package.fee()?);
    Ok(lift.max(fee::fee_for(relay_floor, child_weight)?))
}

#[derive(Debug, Clone)]
pub(crate) struct ChildRequest<'a> {
    pub(crate) rate: FeeRate,
    /// The parent's sources, the address the child pays first.
    pub(crate) sources: Vec<Source<'a>>,
    /// The parent's change, which the child spends first.
    pub(crate) change: &'a Utxo,
    /// Coins of the sources the child may add when the change cannot pay. Only a
    /// confirmed one is added, because an unconfirmed one brings ancestors of its own
    /// that the package does not price.
    pub(crate) extras: &'a [Utxo],
    pub(crate) tip_height: u32,
    pub(crate) relay_floor: FeeRate,
}

/// The child spending the parent's change, with confirmed coins added one at a time
/// until what is left after the fee is worth an output, paying it all back to the
/// first source.
pub(crate) fn build_child(
    params: &UtxoParams,
    package: &Package,
    request: &ChildRequest<'_>,
) -> Result<BuiltTx, UtxoError> {
    let current = package.rate()?;
    if request.rate <= current {
        return Err(UtxoError::RateNotIncreased {
            txid: package.parent(),
            old: current,
            unit: params.fee_rate_unit,
        });
    }
    fee::check_relay_floor(params, request.rate, request.relay_floor)?;
    let to = request.sources.first().ok_or(UtxoError::payload("a child names no source"))?;
    let dust = params.dust_threshold(to.script.script_pubkey());
    let mut extras = ordered_candidates(request.extras, request.rate, Eligible::Confirmed)
        .into_iter()
        .filter(|coin| {
            coin.outpoint() != request.change.outpoint()
                && coin.is_selectable(params, request.tip_height)
        });

    let mut inputs = vec![request.change.clone()];
    let (weight, child, value) = loop {
        let weight =
            weight::predict_weight(inputs.iter().map(Utxo::kind), [to.script.script_pubkey()]);
        let child = child_fee(package, request.rate, request.relay_floor, weight)?;
        let available = Utxo::total(&inputs)?;
        if let Some(value) = available.checked_sub(child).filter(|value| *value >= dust) {
            break (weight, child, value);
        }
        let Some(coin) = extras.next() else {
            return Err(UtxoError::InsufficientFunds {
                needed: child.saturating_add(dust),
                available,
            });
        };
        inputs.push(coin);
    };
    weight::check_standard_weight(params, weight)?;
    fee::check_fee_ceiling(params, child)?;
    let tx = assemble(
        params.replacement.input_sequence(),
        &inputs,
        vec![tx_out(value, to.script.script_buf().clone())?],
        request.tip_height,
    )?;
    let payload = UtxoPayload::assemble(
        tx,
        &inputs,
        &Intent {
            sources: participants(&request.sources, &inputs, true),
            to: to.address.to_owned(),
            strategy: Strategy::ChildPaysForParent,
            donated: Amount::ZERO,
            replaces: None,
            pays_for: Some(package.parent()),
        },
    )?;
    let selection = Selection {
        inputs,
        recipient_value: value,
        change: None,
        donated: Amount::ZERO,
        fee: child,
        weight,
        strategy: Strategy::ChildPaysForParent,
    };
    Ok(BuiltTx { payload, selection })
}

#[cfg(test)]
mod tests {
    use std::slice::from_ref;

    use bitcoin::{OutPoint, Transaction};
    use jelly_core::TxWarningKind;

    use super::*;
    use crate::coin::{Origin, units};
    use crate::fixtures::{
        FLOOR_QUOTE, Fallible, ONE_PER_VB, Scene, TEN_PER_VB, TIP, confirmed, kwu, litecoin_scene,
        sent_original,
    };
    use crate::preview::preview;
    use crate::sign::sign_tx;
    use crate::test_util::fixture_txid;

    fn pending(seed: usize, fee: u64, vsize: u64) -> Unconfirmed {
        Unconfirmed { txid: fixture_txid(seed), fee: units(fee), weight: vbytes(vsize) }
    }

    fn vbytes(vsize: u64) -> Weight {
        Weight::from_vb(vsize).unwrap()
    }

    /// Electrum's `qetxfinalizer.py`: the package fee at the target rate over the
    /// parent's size plus the child's, less what the parent paid.
    #[test]
    fn the_child_pays_the_package_rate_over_both_sizes_less_what_the_parent_paid() {
        let package = Package::new(pending(1, 200, 200), Vec::new());
        let child = vbytes(110);
        assert_eq!(child_fee(&package, TEN_PER_VB, ONE_PER_VB, child).unwrap(), units(2_900));
        assert_eq!(package.rate().unwrap(), ONE_PER_VB);
    }

    #[test]
    fn every_unconfirmed_ancestor_is_lifted_with_the_parent() {
        let package = Package::new(pending(1, 200, 200), vec![pending(2, 150, 150)]);
        let child = vbytes(110);
        assert_eq!(
            child_fee(&package, TEN_PER_VB, ONE_PER_VB, child).unwrap(),
            units(4_600 - 350),
            "460 virtual bytes at ten, less the 350 the two already paid"
        );
    }

    #[test]
    fn a_package_that_already_pays_the_rate_still_leaves_the_child_its_relay_fee() {
        let package = Package::new(pending(1, 20_000, 200), Vec::new());
        let child = vbytes(110);
        assert_eq!(child_fee(&package, TEN_PER_VB, ONE_PER_VB, child).unwrap(), units(110));
    }

    #[test]
    fn a_package_fee_rounds_up_so_the_rate_is_reached() {
        let package = Package::new(pending(1, 100, 101), Vec::new());
        let child = vbytes(110);
        let rate = kwu(375);
        assert_eq!(
            child_fee(&package, rate, ONE_PER_VB, child).unwrap(),
            units(217),
            "211 virtual bytes at 1.5 is 316.5, which rounds up to 317, less the 100 paid"
        );
    }

    /// A Litecoin send paying the stranger with change back to us, and the package it
    /// starts.
    struct Parent {
        scene: Scene,
        tx: Transaction,
        package: Package,
        change: Utxo,
    }

    fn parent(funded: u64, paid: u64) -> Fallible<Parent> {
        let scene = litecoin_scene()?;
        let (original, _) = sent_original(&scene, Some(paid), ONE_PER_VB, funded)?;
        let tx = original.tx().clone();
        let change_value =
            tx.output.get(1).ok_or("the parent needs a change output")?.value.to_sat();
        let change = Utxo::new(
            OutPoint { txid: tx.compute_txid(), vout: 1 },
            units(change_value),
            scene.ours.script().clone(),
            None,
            Origin::Own,
        );
        Ok(Parent {
            scene,
            tx,
            package: Package::new(Unconfirmed::from(&original), Vec::new()),
            change,
        })
    }

    impl Parent {
        fn request<'a>(&'a self, rate: FeeRate, extras: &'a [Utxo]) -> ChildRequest<'a> {
            ChildRequest {
                rate,
                sources: self.scene.own_source().to_vec(),
                change: &self.change,
                extras,
                tip_height: TIP,
                relay_floor: self.scene.params.min_relay_fee,
            }
        }
    }

    #[test]
    fn a_child_spends_the_change_and_pays_it_back_less_the_package_fee() {
        let parent = parent(500_000, 100_000).unwrap();
        let built =
            build_child(&parent.scene.params, &parent.package, &parent.request(TEN_PER_VB, &[]))
                .unwrap();
        let tx = built.payload.unsigned_tx();
        assert_eq!(built.payload.outpoints(), [parent.change.outpoint()]);
        assert_eq!(tx.output.len(), 1);
        let output = tx.output.first().unwrap();
        assert_eq!(output.script_pubkey, *parent.scene.ours.script_pubkey());
        let expected =
            child_fee(&parent.package, TEN_PER_VB, ONE_PER_VB, built.selection.weight).unwrap();
        assert_eq!(built.selection.fee, expected);
        assert_eq!(units(output.value.to_sat()), parent.change.value().saturating_sub(expected));
        assert_eq!(built.payload.intent().pays_for, Some(parent.tx.compute_txid()));

        let shown = preview(&parent.scene.params, &built.payload, FLOOR_QUOTE).unwrap();
        assert!(shown.amount.is_zero(), "nothing leaves the wallet but the fee");
        assert_eq!(shown.fee, expected);
        assert!(
            shown.warnings.iter().any(|warning| warning.kind == TxWarningKind::ChildPays
                && warning.message.contains("second transaction")),
            "{:?}",
            shown.warnings
        );
        let signed =
            sign_tx(&parent.scene.params, &built.payload, &[parent.scene.ours.material()]).unwrap();
        assert_eq!(signed.txid(), built.payload.unsigned_txid());
    }

    #[test]
    fn a_change_too_small_to_pay_is_topped_up_from_a_confirmed_coin() {
        let parent = parent(105_000, 100_000).unwrap();
        let extras = [confirmed(&parent.scene.ours, 9, 50_000)];
        let built = build_child(
            &parent.scene.params,
            &parent.package,
            &parent.request(kwu(25_000), &extras),
        )
        .unwrap();
        assert_eq!(built.payload.outpoints().len(), 2);
        assert_eq!(built.payload.outpoints().first(), Some(&parent.change.outpoint()));
    }

    #[test]
    fn a_child_adds_only_the_inputs_needed_to_leave_a_non_dust_output() {
        let parent = parent(105_000, 100_000).unwrap();
        let extras = [
            confirmed(&parent.scene.ours, 5, 18_000),
            confirmed(&parent.scene.ours, 6, 20_000),
            confirmed(&parent.scene.ours, 7, 19_000),
        ];
        let built = build_child(
            &parent.scene.params,
            &parent.package,
            &parent.request(kwu(25_000), &extras),
        )
        .unwrap();
        assert_eq!(
            built.payload.outpoints(),
            [parent.change.outpoint(), extras[1].outpoint(), extras[2].outpoint()]
        );
        assert_eq!(
            built.selection.recipient_value.checked_add(built.selection.fee),
            Some(Utxo::total(&built.selection.inputs).unwrap())
        );
        assert!(
            built.selection.recipient_value
                >= parent.scene.params.dust_threshold(parent.scene.ours.script().script_pubkey())
        );
    }

    #[test]
    fn an_unconfirmed_coin_is_never_added_to_a_child() {
        let parent = parent(105_000, 100_000).unwrap();
        let extra = Utxo::new(
            OutPoint { txid: fixture_txid(9), vout: 0 },
            units(50_000),
            parent.scene.ours.script().clone(),
            None,
            Origin::Own,
        );
        let error = build_child(
            &parent.scene.params,
            &parent.package,
            &parent.request(kwu(25_000), from_ref(&extra)),
        )
        .unwrap_err();
        assert!(matches!(error, UtxoError::InsufficientFunds { .. }), "{error:?}");
    }

    #[test]
    fn a_rate_the_package_already_pays_is_refused() {
        let parent = parent(500_000, 100_000).unwrap();
        let current = parent.package.rate().unwrap();
        let error =
            build_child(&parent.scene.params, &parent.package, &parent.request(current, &[]))
                .unwrap_err();
        assert!(
            matches!(error, UtxoError::RateNotIncreased { txid, .. } if txid == parent.tx.compute_txid()),
            "{error:?}"
        );
    }
}
