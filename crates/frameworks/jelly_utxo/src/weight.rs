use bitcoin::consensus::encode::VarInt;
use bitcoin::transaction::{InputWeightPrediction, predict_weight as predict_signed_weight};
use bitcoin::{Script, Weight};

use crate::error::UtxoError;
use crate::params::UtxoParams;
use crate::script::UtxoScript;

/// The witness and script-signature sizes each script kind spends with, at the
/// largest DER encoding a signature can take, so a prediction never undershoots.
///
/// ```text
/// | kind        | scriptSig | witness items    | input weight |
/// | ----------- | --------- | ---------------- | ------------ |
/// | p2tr        | empty     | 64-byte schnorr  | 230          |
/// | p2wpkh      | empty     | 72 sig, 33 key   | 272          |
/// | p2sh_p2wpkh | 23 bytes  | 72 sig, 33 key   | 364          |
/// | p2pkh       | 107 bytes | none             | 592          |
/// ```
const fn input_prediction(kind: UtxoScript) -> InputWeightPrediction {
    match kind {
        UtxoScript::P2tr => InputWeightPrediction::P2TR_KEY_DEFAULT_SIGHASH,
        UtxoScript::P2wpkh => InputWeightPrediction::P2WPKH_MAX,
        // A single push of the 22-byte `OP_0 <keyhash>` redeem script.
        UtxoScript::P2shP2wpkh => InputWeightPrediction::from_slice(23, &[72, 33]),
        UtxoScript::P2pkh => InputWeightPrediction::P2PKH_COMPRESSED_MAX,
    }
}

/// Weight of one input of `kind`, outpoint and sequence included.
///
/// A `const` table so selection arithmetic needs no prediction. An in-module
/// test pins it to the `bitcoin` predictor.
pub(crate) const fn input_weight(kind: UtxoScript) -> Weight {
    Weight::from_wu(match kind {
        UtxoScript::P2tr => 230,
        UtxoScript::P2wpkh => 272,
        UtxoScript::P2shP2wpkh => 364,
        UtxoScript::P2pkh => 592,
    })
}

/// Weight of one output paying `script_pubkey`, all of it non-witness data.
pub(crate) fn output_weight(script_pubkey: &Script) -> Weight {
    // `usize` is at most 64 bits on every target this workspace builds for, so
    // the fallback is unreachable.
    let length = u64::try_from(script_pubkey.len()).unwrap_or(u64::MAX);
    let prefix = u64::try_from(VarInt(length).size()).unwrap_or(u64::MAX);
    let bytes = 8u64.saturating_add(prefix).saturating_add(length);
    Weight::from_wu(bytes.saturating_mul(Weight::WITNESS_SCALE_FACTOR))
}

/// Total weight of a signed transaction spending `inputs` into `outputs`,
/// predicted before any signature exists.
pub(crate) fn predict_weight<'a>(
    inputs: impl IntoIterator<Item = UtxoScript>,
    outputs: impl IntoIterator<Item = &'a Script>,
) -> Weight {
    predict_signed_weight(
        inputs.into_iter().map(input_prediction),
        outputs.into_iter().map(Script::len),
    )
}

/// Virtual size as a node charges it, the weight divided by four and rounded up.
pub(crate) const fn vsize(weight: Weight) -> u64 {
    weight.to_wu().div_ceil(Weight::WITNESS_SCALE_FACTOR)
}

/// Refuses a transaction the network would not relay for its size alone.
pub(crate) fn check_standard_weight(params: &UtxoParams, weight: Weight) -> Result<(), UtxoError> {
    if weight > params.max_standard_weight {
        return Err(too_heavy(params, weight));
    }
    Ok(())
}

/// The refusal of a transaction of `weight`, in the unit the chain limits it by.
pub(crate) const fn too_heavy(params: &UtxoParams, weight: Weight) -> UtxoError {
    let maximum = params.max_standard_weight;
    if params.has_segwit {
        UtxoError::TooHeavy { weight, maximum }
    } else {
        UtxoError::TooLarge { size_bytes: vsize(weight), maximum_bytes: maximum.to_vbytes_floor() }
    }
}

#[cfg(test)]
mod tests {
    use std::iter::empty;

    use bitcoin::ScriptBuf;

    use super::*;
    use crate::fixtures::Fallible;
    use crate::test_util::{BITCOIN, BITCOIN_CASH, DOGECOIN, test_key};

    #[test]
    fn the_input_weight_table_matches_the_predictor() {
        for kind in UtxoScript::ALL {
            let with = predict_signed_weight([input_prediction(kind)], empty::<usize>());
            let without = predict_signed_weight(empty(), empty::<usize>());
            // The segwit marker and flag belong to the transaction, not the input.
            let overhead = if kind == UtxoScript::P2pkh { 0 } else { 2 };
            let expected = with.to_wu() - without.to_wu() - overhead;
            assert_eq!(input_weight(kind).to_wu(), expected, "{kind:?}");
        }
    }

    fn script_pubkey(kind: UtxoScript) -> Fallible<ScriptBuf> {
        Ok(test_key(&BITCOIN, kind.into(), 1)?.script_pubkey())
    }

    #[test]
    fn an_output_weighs_its_value_its_length_prefix_and_its_script() {
        // A 22-byte P2WPKH script is 8 value + 1 prefix + 22 script bytes, all
        // non-witness.
        assert_eq!(
            output_weight(script_pubkey(UtxoScript::P2wpkh).unwrap().as_script()).to_wu(),
            31 * 4
        );
        // A 34-byte P2TR script.
        assert_eq!(
            output_weight(script_pubkey(UtxoScript::P2tr).unwrap().as_script()).to_wu(),
            43 * 4
        );
        // A 25-byte P2PKH script.
        assert_eq!(
            output_weight(script_pubkey(UtxoScript::P2pkh).unwrap().as_script()).to_wu(),
            34 * 4
        );
    }

    #[test]
    fn a_one_input_two_output_segwit_spend_is_one_hundred_and_forty_one_vbytes() {
        let recipient = script_pubkey(UtxoScript::P2wpkh).unwrap();
        let change = script_pubkey(UtxoScript::P2wpkh).unwrap();
        let weight =
            predict_weight([UtxoScript::P2wpkh], [recipient.as_script(), change.as_script()]);
        assert_eq!(vsize(weight), 141);
    }

    #[test]
    fn a_legacy_only_transaction_carries_no_witness_overhead() {
        let recipient = script_pubkey(UtxoScript::P2pkh).unwrap();
        let legacy = predict_weight([UtxoScript::P2pkh], [recipient.as_script()]);
        let segwit = predict_weight([UtxoScript::P2wpkh], [recipient.as_script()]);
        // Ten non-witness frame bytes for the version, locktime, and two counts, plus
        // one input and one output. Only the segwit form pays the two weight units of
        // marker and flag.
        assert_eq!(legacy.to_wu(), 10 * 4 + 592 + 34 * 4);
        assert_eq!(segwit.to_wu(), 10 * 4 + 2 + 272 + 34 * 4);
    }

    #[test]
    fn virtual_size_rounds_up_the_way_a_node_does() {
        assert_eq!(vsize(Weight::from_wu(0)), 0);
        assert_eq!(vsize(Weight::from_wu(1)), 1);
        assert_eq!(vsize(Weight::from_wu(4)), 1);
        assert_eq!(vsize(Weight::from_wu(5)), 2);
        assert_eq!(vsize(Weight::from_wu(561)), 141);
        assert_eq!(vsize(Weight::from_wu(564)), 141);
    }

    #[test]
    fn a_transaction_above_the_standard_weight_limit_is_refused() {
        let maximum = Weight::from_wu(400_000);
        check_standard_weight(&BITCOIN, maximum).unwrap();
        assert!(matches!(
            check_standard_weight(&BITCOIN, Weight::from_wu(400_001)),
            Err(UtxoError::TooHeavy { .. })
        ));
    }

    /// A chain without segregated witness counts the limit in bytes, a quarter of
    /// the weight, and a size that rounds up past it is still refused.
    #[test]
    fn a_chain_without_segwit_refuses_a_transaction_by_its_size_in_bytes() {
        for params in [BITCOIN_CASH, DOGECOIN] {
            check_standard_weight(&params, params.max_standard_weight).unwrap();
            let error = check_standard_weight(&params, Weight::from_wu(400_001)).unwrap_err();
            assert!(
                matches!(
                    error,
                    UtxoError::TooLarge { size_bytes: 100_001, maximum_bytes: 100_000 }
                ),
                "{error:?}"
            );
            assert_eq!(
                error.to_string(),
                "The transaction is 100001 bytes. The network limit is 100000 bytes."
            );
        }
    }
}
