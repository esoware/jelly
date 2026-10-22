use zcash_primitives::transaction::fees::{
    FeeRule as _,
    transparent::{InputView, OutputView},
    zip317::FeeRule,
};
use zcash_protocol::{
    consensus::{BlockHeight, MAIN_NETWORK},
    value::Zatoshis,
};
use zcash_transparent::{
    address::Script,
    bundle::{OutPoint, TxOut},
};

use crate::address::script;
use crate::error::ZcashError;
use crate::payload::{Coin, INVALID_PAYMENT};

/// A coin as ZIP 317 sizes it, from the script of the address that holds it.
#[derive(Debug)]
struct Spent {
    outpoint: OutPoint,
    coin: TxOut,
}

impl InputView for Spent {
    fn outpoint(&self) -> &OutPoint {
        &self.outpoint
    }

    fn coin(&self) -> &TxOut {
        &self.coin
    }
}

/// ZIP 317's block template weighs a fee at most four times the transaction's
/// conventional fee, so a fee above that confirms no sooner and is refused as a mistake.
pub(crate) const MAX_FEE_MULTIPLE: u64 = 4;

/// What a payment whose ZIP 317 fee is `conventional_zat` pays: that fee for a level, or
/// the custom fee `chosen_zat`, which has to be at least that fee and at most
/// [`MAX_FEE_MULTIPLE`] times it.
pub(crate) const fn paid_fee(
    conventional_zat: u64,
    chosen_zat: Option<u64>,
) -> Result<u64, ZcashError> {
    let Some(chosen_zat) = chosen_zat else {
        return Ok(conventional_zat);
    };
    if chosen_zat < conventional_zat {
        return Err(ZcashError::FeeBelowMinimum {
            fee_zat: chosen_zat,
            minimum_zat: conventional_zat,
        });
    }
    let maximum_zat = conventional_zat.saturating_mul(MAX_FEE_MULTIPLE);
    if chosen_zat > maximum_zat {
        return Err(ZcashError::FeeAboveMaximum { maximum_zat });
    }
    Ok(chosen_zat)
}

/// The ZIP 317 fee of a transaction spending `inputs` into one output for each of
/// `outputs`, at `target_height`.
///
/// Each input is sized by the script it spends, so a coin of any script but a key hash
/// is refused rather than priced as one.
pub(crate) fn conventional_fee(
    target_height: u32,
    inputs: &[Coin],
    outputs: &[&Script],
) -> Result<u64, ZcashError> {
    let mut spent = Vec::with_capacity(inputs.len());
    for coin in inputs {
        spent.push(Spent {
            outpoint: coin.outpoint()?,
            coin: TxOut::new(
                Zatoshis::from_u64(coin.value_zat).map_err(|_| ZcashError::Protocol)?,
                script(&coin.address)?,
            ),
        });
    }
    let sized: Vec<TxOut> =
        outputs.iter().map(|script| TxOut::new(Zatoshis::ZERO, (*script).clone())).collect();
    FeeRule::standard()
        .fee_required(
            &MAIN_NETWORK,
            BlockHeight::from_u32(target_height),
            spent.iter().map(InputView::serialized_size),
            sized.iter().map(OutputView::serialized_size),
            0,
            0,
            0,
            0,
        )
        .map(u64::from)
        .map_err(|_| INVALID_PAYMENT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{OWNER, RECIPIENT, SCRIPT_HASH};

    const HEIGHT: u32 = 3_514_159;

    fn coins(address: &str, count: u8) -> Vec<Coin> {
        (1..=count)
            .map(|seed| Coin {
                txid: format!("{seed:02x}").repeat(32),
                index: 0,
                value_zat: 10_000,
                address: address.to_owned(),
            })
            .collect()
    }

    /// ZIP 317's own figures.
    ///
    /// Two grace actions, then one marginal fee per logical action, which for
    /// transparent parts is the larger of the inputs counted in 150-byte P2PKH inputs
    /// and the outputs counted in 34-byte P2PKH outputs.
    #[test]
    fn the_fee_is_zip_317s_for_the_inputs_and_outputs_it_is_given() {
        let recipient = script(RECIPIENT).unwrap();
        let change = script(OWNER).unwrap();
        for (count, fee_zat) in [(1, 10_000), (2, 10_000), (3, 15_000), (7, 35_000)] {
            let inputs = coins(OWNER, count);
            assert_eq!(
                conventional_fee(HEIGHT, &inputs, &[&recipient, &change]).unwrap(),
                fee_zat,
                "{count} inputs"
            );
        }
        assert_eq!(conventional_fee(HEIGHT, &coins(OWNER, 1), &[&recipient; 3]).unwrap(), 15_000);
        let p2sh = script(SCRIPT_HASH).unwrap();
        assert_eq!(conventional_fee(HEIGHT, &coins(OWNER, 1), &[&p2sh]).unwrap(), 10_000);
    }

    #[test]
    fn a_custom_fee_has_to_clear_the_zip_317_fee_and_stay_within_four_times_it() {
        assert_eq!(paid_fee(10_000, None).unwrap(), 10_000);
        assert_eq!(paid_fee(10_000, Some(10_000)).unwrap(), 10_000);
        assert_eq!(paid_fee(10_000, Some(40_000)).unwrap(), 40_000);
        assert!(matches!(
            paid_fee(10_000, Some(9_999)),
            Err(ZcashError::FeeBelowMinimum { fee_zat: 9_999, minimum_zat: 10_000 })
        ));
        assert!(matches!(
            paid_fee(10_000, Some(40_001)),
            Err(ZcashError::FeeAboveMaximum { maximum_zat: 40_000 })
        ));
        assert_eq!(
            paid_fee(10_000, Some(9_999)).unwrap_err().to_string(),
            "The fee of 0.00009999 ZEC is below the ZIP 317 fee of 0.0001 ZEC for this payment."
        );
    }

    /// A coin of a script-hash address has a redeem script ZIP 317 cannot size, so it
    /// is refused rather than underpriced as a key-hash input.
    #[test]
    fn a_coin_zip_317_cannot_size_is_refused_rather_than_priced_as_a_key_hash() {
        let recipient = script(RECIPIENT).unwrap();
        let error = conventional_fee(HEIGHT, &coins(SCRIPT_HASH, 1), &[&recipient]).unwrap_err();
        assert!(matches!(error, ZcashError::Invalid(_)), "{error:?}");
    }
}
