use bitcoin::absolute::LockTime;
use bitcoin::transaction::Version;
use bitcoin::{Script, ScriptBuf, Sequence, Transaction, TxIn, TxOut, Witness};
use jelly_core::{Amount, FeeRate};

use crate::coin::{Utxo, tx_out};
use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::payload::{Intent, UtxoPayload};
use crate::script::SpendableScript;
use crate::select::{Selection, SelectionRequest, select_coins};
use crate::{fee, weight};

/// A sending address, canonical and already through the table's address gate.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Source<'a> {
    pub(crate) address: &'a str,
    pub(crate) script: &'a SpendableScript,
}

/// The sources `inputs` spend from, in order, plus the first when change returns to
/// it. The payload names these, and their keys sign it.
pub(crate) fn participants(
    sources: &[Source<'_>],
    inputs: &[Utxo],
    has_change: bool,
) -> Vec<String> {
    sources
        .iter()
        .enumerate()
        .filter(|&(position, source)| {
            (has_change && position == 0)
                || inputs.iter().any(|coin| coin.script_pubkey() == source.script.script_pubkey())
        })
        .map(|(_, source)| source.address.to_owned())
        .collect()
}

/// Both ends are already canonical and through the table's address gate, so a
/// build parses no address.
#[derive(Debug, Clone)]
pub(crate) struct BuildRequest<'a> {
    /// The addresses whose coins pool into `coins`, in the request's order. Change
    /// goes to the first.
    pub(crate) sources: Vec<Source<'a>>,
    pub(crate) to: &'a str,
    pub(crate) to_script: &'a Script,
    /// `None` for a send-max.
    pub(crate) amount: Option<Amount>,
    pub(crate) fee_rate: FeeRate,
    /// Every source's verified unspent coins, minus claimed ones.
    pub(crate) coins: &'a [Utxo],
    /// The tip the anti-fee-sniping locktime is set from, and the one a coinbase
    /// or peg-out output has to have matured by.
    pub(crate) tip_height: u32,
    /// The lowest rate the network currently relays, never under the table's
    /// compiled floor.
    pub(crate) relay_floor: FeeRate,
}

#[derive(Debug, Clone)]
pub(crate) struct BuiltTx {
    pub(crate) payload: UtxoPayload,
    pub(crate) selection: Selection,
}

/// Does no I/O. Everything the network had to say is already in `request`.
pub(crate) fn build_tx(
    params: &UtxoParams,
    request: &BuildRequest<'_>,
) -> Result<BuiltTx, UtxoError> {
    fee::check_relay_floor(params, request.fee_rate, request.relay_floor)?;
    let change = request.sources.first().ok_or(UtxoError::payload("a build names no source"))?;

    let target = match request.amount {
        None => Amount::ZERO,
        Some(amount) => {
            let dust = params.dust_threshold(request.to_script);
            if amount < dust {
                return Err(UtxoError::Dust {
                    value: amount,
                    threshold: dust,
                    chain: params.chain,
                });
            }
            amount
        }
    };

    // A node refuses the whole transaction if any input is an immature coinbase or
    // peg-out output, and spending an input that carries tokens would burn them.
    let selectable: Vec<Utxo> = request
        .coins
        .iter()
        .filter(|coin| coin.is_selectable(params, request.tip_height))
        .cloned()
        .collect();
    let selection = select_coins(
        params,
        &SelectionRequest {
            target,
            should_drain: request.amount.is_none(),
            fee_rate: request.fee_rate,
            long_term_fee_rate: params.long_term_fee_rate,
            recipient_script_pubkey: request.to_script,
            change_script: change.script,
        },
        &selectable,
    )?;

    // The recipient is output zero and change, if any, is output one. Outputs are
    // never shuffled.
    let mut outputs = vec![tx_out(selection.recipient_value, request.to_script.to_owned())?];
    if let Some(value) = selection.change {
        outputs.push(tx_out(value, change.script.script_buf().clone())?);
    }

    let tx = assemble(
        params.replacement.input_sequence(),
        &selection.inputs,
        outputs,
        request.tip_height,
    )?;
    weight::check_standard_weight(params, selection.weight)?;
    fee::check_fee_ceiling(params, selection.fee)?;

    let payload = UtxoPayload::assemble(
        tx,
        &selection.inputs,
        &Intent {
            sources: participants(&request.sources, &selection.inputs, selection.change.is_some()),
            to: request.to.to_owned(),
            strategy: selection.strategy,
            donated: selection.donated,
            replaces: None,
            pays_for: None,
        },
    )?;
    Ok(BuiltTx { payload, selection })
}

/// Version 2, every input at the chain's sequence, and an anti-fee-sniping
/// locktime at the current tip.
///
/// The locktime keeps the transaction out of a reorged earlier block and matches
/// what other wallets set.
pub(crate) fn assemble(
    sequence: Sequence,
    inputs: &[Utxo],
    outputs: Vec<TxOut>,
    tip_height: u32,
) -> Result<Transaction, UtxoError> {
    let lock_time = LockTime::from_height(tip_height)
        .map_err(|_| UtxoError::InvalidTip { height: tip_height })?;
    Ok(Transaction {
        version: Version::TWO,
        lock_time,
        input: inputs
            .iter()
            .map(|coin| TxIn {
                previous_output: coin.outpoint(),
                script_sig: ScriptBuf::new(),
                sequence,
                witness: Witness::new(),
            })
            .collect(),
        output: outputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coin::units;
    use crate::fixtures::{
        ONE_PER_VB, TEN_PER_VB, TIP, bitcoin_cash_scene, bitcoin_scene, confirmed, litecoin_scene,
        scene,
    };
    use crate::script::UtxoScript;
    use crate::select::Strategy;
    use crate::test_util::{BITCOIN, LITECOIN};
    use crate::weight::predict_weight;

    #[test]
    fn a_bitcoin_transaction_signals_replacement_and_carries_the_anti_fee_sniping_locktime() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let tx = built.payload.unsigned_tx();

        assert_eq!(tx.version.0, 2);
        assert_eq!(tx.lock_time.to_consensus_u32(), TIP);
        assert!(tx.input.iter().all(|txin| txin.sequence == BITCOIN.replacement.input_sequence()));
        assert!(tx.input.iter().all(|txin| txin.sequence.is_rbf()));
    }

    /// Bitcoin Cash has no replacement, so its inputs don't signal, but the locktime
    /// is still enforced.
    #[test]
    fn a_bitcoin_cash_transaction_signals_no_replacement_and_keeps_its_locktime() {
        let scene = bitcoin_cash_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let tx = built.payload.unsigned_tx();
        assert_eq!(tx.lock_time.to_consensus_u32(), TIP);
        assert!(tx.input.iter().all(|txin| !txin.sequence.is_rbf()));
        assert!(tx.input.iter().all(|txin| txin.sequence.enables_absolute_lock_time()));
    }

    /// A coin that carries tokens is never an input, so a payment can't burn them.
    #[test]
    fn a_coin_that_carries_tokens_is_never_spent() {
        let scene = bitcoin_cash_scene().unwrap();
        let mut coins = scene.coins(&[200_000, 80_000]);
        if let Some(first) = coins.first_mut() {
            *first = first.clone().holding_tokens(true);
        }
        let built = scene.build(Some(50_000), ONE_PER_VB, &coins).unwrap();
        assert_eq!(built.selection.inputs.len(), 1);
        assert!(built.selection.inputs.iter().all(|coin| !coin.has_tokens()));
        assert!(matches!(
            scene.build(Some(100_000), ONE_PER_VB, &coins),
            Err(UtxoError::InsufficientFunds { .. })
        ));
        let drained = scene.build(None, ONE_PER_VB, &coins).unwrap();
        assert!(drained.selection.inputs.iter().all(|coin| !coin.has_tokens()));
    }

    #[test]
    fn the_recipient_is_output_zero_and_the_change_comes_back_to_the_sender() {
        let scene = bitcoin_scene().unwrap();
        let built = scene.build(Some(50_000), ONE_PER_VB, &scene.coins(&[200_000])).unwrap();
        let outputs = &built.payload.unsigned_tx().output;

        assert_eq!(outputs.len(), 2);
        let recipient = outputs.first().expect("a payment");
        let change = outputs.get(1).expect("change");
        assert_eq!(recipient.script_pubkey, scene.theirs.script_pubkey());
        assert_eq!(recipient.value.to_sat(), 50_000);
        assert_eq!(change.script_pubkey, scene.ours.script_pubkey());
        assert_eq!(
            built.selection.change.map(Amount::base_units),
            Some(u128::from(change.value.to_sat()))
        );
    }

    #[test]
    fn every_input_of_the_payload_carries_the_output_it_spends() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[80_000, 90_000]);
        let built = scene.build(Some(120_000), ONE_PER_VB, &coins).unwrap();

        assert_eq!(built.payload.inputs().len(), 2);
        for (input, coin) in built.payload.inputs().iter().zip(&built.selection.inputs) {
            assert_eq!(input.outpoint, coin.outpoint());
            assert_eq!(input.value, coin.value());
            assert_eq!(input.script.script_pubkey(), coin.script_pubkey());
            assert_eq!(input.script.kind(), coin.kind());
        }
        assert_eq!(
            built.payload.outpoints(),
            built.selection.inputs.iter().map(Utxo::outpoint).collect::<Vec<_>>()
        );
    }

    #[test]
    fn building_is_deterministic() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[70_000, 80_000, 90_000]);
        let first = scene.build(Some(100_000), ONE_PER_VB, &coins).unwrap();
        let second = scene.build(Some(100_000), ONE_PER_VB, &coins).unwrap();
        assert_eq!(first.payload, second.payload);
        assert_eq!(first.payload.encode(), second.payload.encode());
    }

    #[test]
    fn all_four_script_kinds_build_and_predict_their_own_weight() {
        for kind in UtxoScript::ALL {
            let scene = scene(&BITCOIN, kind.into()).unwrap();
            let coins = scene.coins(&[500_000]);
            let built = scene.build(Some(100_000), ONE_PER_VB, &coins).unwrap();
            assert_eq!(built.payload.inputs().first().map(|input| input.script.kind()), Some(kind));
            let expected = predict_weight(
                [kind],
                [scene.theirs.script().script_pubkey(), scene.ours.script().script_pubkey()],
            );
            assert_eq!(built.payload.predicted_weight(), expected, "{kind:?}");
            assert_eq!(built.selection.weight, expected, "{kind:?}");
        }
    }

    #[test]
    fn a_payment_below_the_dust_threshold_is_refused() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[200_000]);
        assert!(matches!(scene.build(Some(293), ONE_PER_VB, &coins), Err(UtxoError::Dust { .. })));
        scene.build(Some(294), ONE_PER_VB, &coins).unwrap();
    }

    #[test]
    fn litecoin_refuses_a_payment_bitcoin_would_have_allowed() {
        let btc = bitcoin_scene().unwrap();
        let ltc = litecoin_scene().unwrap();
        btc.build(Some(1_000), ONE_PER_VB, &btc.coins(&[200_000])).unwrap();
        assert!(matches!(
            ltc.build(Some(1_000), ONE_PER_VB, &ltc.coins(&[200_000])),
            Err(UtxoError::Dust { .. })
        ));
        assert_eq!(LITECOIN.dust_threshold(ltc.theirs.script().script_pubkey()), units(2_940));
    }

    #[test]
    fn a_rate_below_what_the_network_is_relaying_at_is_refused_before_anything_is_built() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[200_000]);
        let mut request = scene.request(Some(50_000), ONE_PER_VB, &coins);
        request.relay_floor = TEN_PER_VB;
        assert!(matches!(
            build_tx(&scene.params, &request),
            Err(UtxoError::BelowRelayFloor { .. })
        ));
    }

    #[test]
    fn a_fee_above_the_ceiling_is_refused_until_the_table_raises_it() {
        let scene = bitcoin_scene().unwrap();
        // Every coin is worth spending at this rate, and the resulting fee is absurd.
        let coins: Vec<_> =
            (1..=40u8).map(|seed| confirmed(&scene.ours, seed, 10_000_000)).collect();
        let rate = FeeRate::from_sat_per_kwu(2_000_000);
        let mut request = scene.request(Some(50_000_000), rate, &coins);
        request.relay_floor = ONE_PER_VB;
        let refused = build_tx(&scene.params, &request);
        assert!(
            matches!(refused, Err(UtxoError::FeeCeiling { .. })),
            "{:?}",
            refused.map(|built| built.selection.fee)
        );
        let raised = UtxoParams { fee_ceiling: Amount::MAX, ..scene.params };
        let built = build_tx(&raised, &request).unwrap();
        assert!(built.selection.fee > BITCOIN.fee_ceiling);
    }

    #[test]
    fn a_send_max_spends_everything_and_makes_no_change() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[70_000, 30_000]);
        let built = scene.build(None, ONE_PER_VB, &coins).unwrap();

        assert_eq!(built.selection.strategy, Strategy::Drain);
        assert_eq!(built.selection.inputs.len(), 2);
        assert_eq!(built.payload.unsigned_tx().output.len(), 1);
        assert_eq!(built.selection.change, None);
        assert_eq!(
            built.selection.recipient_value.checked_add(built.payload.fee()),
            Some(units(100_000))
        );
    }

    #[test]
    fn a_tip_that_is_not_a_block_height_is_refused() {
        let scene = bitcoin_scene().unwrap();
        let coins = scene.coins(&[200_000]);
        let mut request = scene.request(Some(50_000), ONE_PER_VB, &coins);
        request.tip_height = 600_000_000;
        assert!(matches!(build_tx(&scene.params, &request), Err(UtxoError::InvalidTip { .. })));
    }
}
