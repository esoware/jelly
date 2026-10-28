use bitcoin::Weight;
use jelly_core::{Amount, FeeRate};
use jelly_electrum::HistogramBucket;

use crate::error::UtxoError;
use crate::params::{Auction, UtxoParams};
use crate::script::SpendableScript;
use crate::weight;

const KILO: u128 = 1000;

/// Fee for a transaction of `weight` at `rate`, computed the way a node does.
///
/// A node charges over the integer virtual size, so the weight is rounded up to a
/// whole virtual byte and the division rounds up too. Charging over the raw
/// weight can land one base unit below what the relay check demands.
pub(crate) fn fee_for(rate: FeeRate, weight: Weight) -> Result<Amount, UtxoError> {
    let per_kvb = u128::from(rate.sat_per_kwu())
        .checked_mul(u128::from(Weight::WITNESS_SCALE_FACTOR))
        .ok_or_else(|| UtxoError::overflow("a fee rate per kvb"))?;
    let product = per_kvb
        .checked_mul(u128::from(weight::vsize(weight)))
        .ok_or_else(|| UtxoError::overflow("a fee"))?;
    Ok(Amount::from_base_units(product.div_ceil(KILO)))
}

/// The rate a transaction of `weight` that paid `fee` actually achieved, rounded
/// down so a preview never claims more than was paid.
pub(crate) fn rate_for(fee: Amount, weight: Weight) -> Result<FeeRate, UtxoError> {
    let vsize = u128::from(weight::vsize(weight));
    if vsize == 0 {
        return Ok(FeeRate::ZERO);
    }
    let per_kvb = fee
        .base_units()
        .checked_mul(KILO)
        .ok_or_else(|| UtxoError::overflow("a fee rate"))?
        .div_euclid(vsize);
    let per_kwu = per_kvb.div_euclid(u128::from(Weight::WITNESS_SCALE_FACTOR));
    Ok(FeeRate::from_sat_per_kwu(u64::try_from(per_kwu).unwrap_or(u64::MAX)))
}

/// The lowest rate whose [`fee_for`] over `weight` reaches `floor`.
///
/// A replacement floor needs this rather than [`rate_for`], because a rate from a
/// rounded-down division wouldn't pay the fee back. Since [`fee_for`] rounds up, the
/// answer is one more than a floored division of `floor - 1`.
pub(crate) fn rate_that_pays(floor: Amount, weight: Weight) -> Result<FeeRate, UtxoError> {
    let charged_wu = u128::from(weight::vsize(weight))
        .checked_mul(u128::from(Weight::WITNESS_SCALE_FACTOR))
        .ok_or_else(|| UtxoError::overflow("a charged weight"))?;
    if charged_wu == 0 || floor.is_zero() {
        return Ok(FeeRate::ZERO);
    }
    let below = floor
        .base_units()
        .saturating_sub(1)
        .checked_mul(KILO)
        .ok_or_else(|| UtxoError::overflow("a replacement fee floor"))?;
    let per_kwu = below
        .div_euclid(charged_wu)
        .checked_add(1)
        .ok_or_else(|| UtxoError::overflow("a replacement fee rate"))?;
    u64::try_from(per_kwu)
        .map(FeeRate::from_sat_per_kwu)
        .map_err(|_| UtxoError::overflow("a replacement fee rate"))
}

/// The rate of the bucket where the backlog reaches `target_blocks` of the market's
/// blocks, from `[fee rate, total vsize]` buckets in descending rate order.
pub(crate) fn rate_from_histogram(
    auction: &Auction,
    target_blocks: u16,
    buckets: &[HistogramBucket],
) -> Option<FeeRate> {
    let lowest = buckets.last()?.fee_rate;
    let target = u64::from(target_blocks).saturating_mul(auction.block_vsize);
    let mut cumulative = 0u64;
    for bucket in buckets {
        cumulative = cumulative.saturating_add(bucket.vsize);
        if cumulative >= target {
            return Some(bucket.fee_rate);
        }
    }
    // The whole mempool fits inside the target, so its cheapest transaction is
    // already fast enough.
    Some(lowest)
}

/// The fee to create a change output now plus the projected fee to spend it
/// later, the change term of the waste metric.
pub(crate) fn cost_of_change(
    rate: FeeRate,
    long_term_rate: FeeRate,
    change: &SpendableScript,
) -> Result<Amount, UtxoError> {
    let make = fee_for(rate, weight::output_weight(change.script_pubkey()))?;
    let spend = fee_for(long_term_rate, weight::input_weight(change.kind()))?;
    make.checked_add(spend).ok_or_else(|| UtxoError::overflow("the cost of change"))
}

/// The smallest change output worth creating, the higher of the dust threshold
/// and one base unit more than spending it later at `long_term_rate` costs.
///
/// A change value is already net of the fee for creating its output, so only the
/// later spend is weighed against it, as in Bitcoin Core's `min_viable_change`.
/// Counting the creation fee here too would hand the miner up to one more output's
/// fee.
///
/// Coin selection and the replacement planner both use it, the branch-and-bound
/// window mirrors it, and a leftover below it goes to the miner.
pub(crate) fn change_floor(
    params: &UtxoParams,
    long_term_rate: FeeRate,
    change: &SpendableScript,
) -> Result<Amount, UtxoError> {
    let dust = params.dust_threshold(change.script_pubkey());
    let spend = fee_for(long_term_rate, weight::input_weight(change.kind()))?;
    let viable = spend
        .checked_add(Amount::from_base_units(1))
        .ok_or_else(|| UtxoError::overflow("the change floor"))?;
    Ok(viable.max(dust))
}

/// The smallest absolute fee a replacement may pay under BIP-125 rules 3 and 4:
/// what every transaction it evicts paid, plus the incremental relay fee over the
/// replacement's own size.
pub(crate) fn replacement_floor(
    params: &UtxoParams,
    evicted_fee: Amount,
    replacement_weight: Weight,
) -> Result<Amount, UtxoError> {
    let increment = fee_for(params.incremental_relay_fee, replacement_weight)?;
    evicted_fee.checked_add(increment).ok_or_else(|| UtxoError::overflow("a replacement fee floor"))
}

/// The ceiling turns a fee bug or an absurd estimate into an error instead of a
/// loss.
pub(crate) fn check_fee_ceiling(params: &UtxoParams, fee: Amount) -> Result<(), UtxoError> {
    if fee > params.fee_ceiling {
        return Err(UtxoError::FeeCeiling {
            fee,
            ceiling: params.fee_ceiling,
            chain: params.chain,
        });
    }
    Ok(())
}

pub(crate) fn check_relay_floor(
    params: &UtxoParams,
    rate: FeeRate,
    floor: FeeRate,
) -> Result<(), UtxoError> {
    if rate < floor {
        return Err(UtxoError::BelowRelayFloor { rate, floor, unit: params.fee_rate_unit });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::coin::units;
    use crate::fixtures::{ONE_PER_VB, TEN_PER_VB, bitcoin_scene, kwu, litecoin_scene};
    use crate::params::FeeMarket;
    use crate::script::UtxoScript;
    use crate::test_util::{BITCOIN, LITECOIN};
    use crate::weight::{input_weight, output_weight, predict_weight, vsize};

    const LITECOIN_FEE_RATES: &str = include_str!("../tests/vectors/litecoin/fee_rates.json");

    /// The market a table prices from, which only an auctioned chain has.
    fn auction(params: &UtxoParams) -> Auction {
        match params.fee_market {
            FeeMarket::Auction(auction) => auction,
            FeeMarket::Flat => panic!("{} has a flat fee market", params.chain),
        }
    }

    /// The rate the normal level reads from `buckets` on `params`'s market.
    fn normal_rate(params: &UtxoParams, buckets: &[HistogramBucket]) -> Option<FeeRate> {
        let market = auction(params);
        let [_, normal, _] = market.target_blocks;
        rate_from_histogram(&market, normal, buckets)
    }

    fn number(value: &Value, key: &str) -> u64 {
        value.get(key).and_then(Value::as_u64).unwrap_or_default()
    }

    const fn bucket(sat_per_vb: u64, vsize: u64) -> HistogramBucket {
        HistogramBucket { fee_rate: kwu(sat_per_vb.saturating_mul(250)), vsize }
    }

    #[test]
    fn a_fee_rounds_up_so_any_nonzero_rate_pays_something() {
        // 141 vB at one per vB.
        assert_eq!(fee_for(ONE_PER_VB, Weight::from_wu(561)).unwrap(), units(141));
        assert_eq!(fee_for(ONE_PER_VB, Weight::from_wu(564)).unwrap(), units(141));
        assert_eq!(
            fee_for(kwu(1), Weight::from_wu(4)).unwrap(),
            units(1),
            "any nonzero rate pays something"
        );
        assert_eq!(fee_for(FeeRate::ZERO, Weight::from_wu(4_000)).unwrap(), Amount::ZERO);
    }

    #[test]
    fn an_achieved_rate_rounds_down_so_a_preview_never_overstates_it() {
        assert_eq!(rate_for(units(141), Weight::from_wu(564)).unwrap(), ONE_PER_VB);
        // One unit short of two per vB rounds down to 1.992.
        assert_eq!(
            rate_for(units(281), Weight::from_wu(564)).unwrap().format_sat_per_vb(),
            "1.992"
        );
        assert_eq!(rate_for(Amount::ZERO, Weight::ZERO).unwrap(), FeeRate::ZERO);
    }

    #[test]
    fn the_rate_that_pays_a_floor_actually_pays_it() {
        for wu in [230u64, 561, 564, 1_000, 40_000] {
            for floor in [1u64, 141, 500, 12_345] {
                let weight = Weight::from_wu(wu);
                let rate = rate_that_pays(units(floor), weight).unwrap();
                assert!(
                    fee_for(rate, weight).unwrap() >= units(floor),
                    "{wu} wu at a floor of {floor} produced {rate:?}"
                );
                if rate.sat_per_kwu() > 0 {
                    let lower = kwu(rate.sat_per_kwu() - 1);
                    assert!(
                        fee_for(lower, weight).unwrap() < units(floor),
                        "{wu} wu at a floor of {floor}: {lower:?} would also have done"
                    );
                }
            }
        }
    }

    #[test]
    fn the_histogram_reads_the_rate_that_clears_the_chains_target() {
        // One block of mempool at each rate, highest first.
        let buckets = [
            bucket(50, 1_000_000),
            bucket(20, 1_000_000),
            bucket(10, 1_000_000),
            bucket(5, 1_000_000),
            bucket(2, 1_000_000),
            bucket(1, 1_000_000),
        ];
        // Bitcoin's normal target is three blocks and Litecoin's six.
        assert_eq!(normal_rate(&BITCOIN, &buckets), Some(kwu(10 * 250)));
        assert_eq!(normal_rate(&LITECOIN, &buckets), Some(kwu(250)));

        let market = auction(&BITCOIN);
        let [slow, _, fast] = market.target_blocks;
        assert_eq!(rate_from_histogram(&market, slow, &buckets), Some(kwu(250)));
        assert_eq!(rate_from_histogram(&market, fast, &buckets), Some(kwu(50 * 250)));
    }

    #[test]
    fn a_mempool_that_fits_inside_the_target_costs_its_cheapest_rate() {
        let buckets = [bucket(9, 10), bucket(3, 10)];
        assert_eq!(normal_rate(&BITCOIN, &buckets), Some(kwu(3 * 250)));
    }

    #[test]
    fn an_empty_histogram_has_no_rate_to_offer() {
        assert_eq!(normal_rate(&BITCOIN, &[]), None);
    }

    #[test]
    fn a_rate_at_the_floor_passes_and_one_below_it_is_refused() {
        check_relay_floor(&BITCOIN, kwu(250), kwu(250)).unwrap();
        assert!(matches!(
            check_relay_floor(&BITCOIN, kwu(249), kwu(250)),
            Err(UtxoError::BelowRelayFloor { .. })
        ));
    }

    #[test]
    fn a_rate_below_the_floor_is_refused_in_the_chains_own_unit() {
        let refused =
            |params| check_relay_floor(params, kwu(125), kwu(250)).unwrap_err().to_string();
        assert_eq!(
            refused(&BITCOIN),
            "The fee rate of 0.5 sat/vB is below the network minimum of 1 sat/vB."
        );
        assert_eq!(
            refused(&LITECOIN),
            "The fee rate of 0.5 lit/vB is below the network minimum of 1 lit/vB."
        );
    }

    #[test]
    fn the_change_floor_is_the_dust_threshold_or_what_spending_change_costs() {
        let scene = bitcoin_scene().unwrap();
        let change = scene.ours.script();
        let dust = scene.params.dust_threshold(change.script_pubkey());
        assert_eq!(dust, units(294));

        // At one per vB the dust rule dominates.
        let low = change_floor(&scene.params, ONE_PER_VB, change).unwrap();
        assert_eq!(low, dust);

        // At a high long-term rate, spending the output later costs more than the
        // dust threshold. A P2WPKH input is 68 vB.
        let high = change_floor(&scene.params, TEN_PER_VB, change).unwrap();
        assert_eq!(high, units(681));
        assert!(high > dust, "{high} must exceed {dust}");
        assert_eq!(
            Some(high),
            fee_for(TEN_PER_VB, input_weight(change.kind())).unwrap().checked_add(units(1)),
            "the creation fee is already paid out of the change, so it is not counted again"
        );

        // The cost of change, which the waste metric reads, is the output now plus
        // the input later.
        let cost = cost_of_change(TEN_PER_VB, TEN_PER_VB, change).unwrap();
        let expected = fee_for(TEN_PER_VB, output_weight(change.script_pubkey()))
            .unwrap()
            .checked_add(fee_for(TEN_PER_VB, input_weight(change.kind())).expect("a fee"))
            .expect("a total");
        assert_eq!(cost, expected);
    }

    #[test]
    fn litecoin_holds_change_to_a_ten_times_higher_floor() {
        let btc = bitcoin_scene().unwrap();
        let ltc = litecoin_scene().unwrap();
        let btc_floor = change_floor(&btc.params, ONE_PER_VB, btc.ours.script()).unwrap();
        let ltc_floor = change_floor(&ltc.params, ONE_PER_VB, ltc.ours.script()).unwrap();
        assert_eq!(btc_floor, units(294));
        assert_eq!(ltc_floor, units(2_940));
    }

    /// BIP-125 rule 4 requires the original's fee plus the incremental relay fee over
    /// the replacement's own size.
    #[test]
    fn the_replacement_floor_follows_rule_four() {
        let weight = Weight::from_wu(564);
        let floor = replacement_floor(&BITCOIN, units(1_000), weight).unwrap();
        // 141 virtual bytes at 0.1 sat/vB is 14.1, rounded up.
        assert_eq!(floor, units(1_000 + 15));
        assert!(floor > units(1_000), "rule 4 needs a strictly higher fee");

        let raised = BITCOIN.with_incremental_relay_fee(Some(kwu(2_500)));
        assert_eq!(replacement_floor(&raised, units(1_000), weight).unwrap(), units(1_000 + 1_410));
    }

    #[test]
    fn the_absolute_fee_ceiling_is_the_tables_own() {
        assert_eq!(BITCOIN.fee_ceiling, units(2_000_000));
        check_fee_ceiling(&BITCOIN, units(2_000_000)).unwrap();
        assert!(matches!(
            check_fee_ceiling(&BITCOIN, units(2_000_001)),
            Err(UtxoError::FeeCeiling { .. })
        ));
        let raised = UtxoParams { fee_ceiling: units(3_000_000), ..BITCOIN };
        check_fee_ceiling(&raised, units(2_000_001)).unwrap();
    }

    #[test]
    fn a_fee_and_a_rate_round_trip_through_a_weight() {
        let weight = predict_weight(
            [UtxoScript::P2wpkh],
            [bitcoin_scene().unwrap().theirs.script().script_pubkey()],
        );
        for rate in [ONE_PER_VB, TEN_PER_VB, kwu(1), kwu(37_500)] {
            let fee = fee_for(rate, weight).unwrap();
            let back = rate_for(fee, weight).unwrap();
            // The fee rounds up, so the rate a transaction achieved is never below the
            // one that was asked for.
            assert!(back >= rate, "{back:?} must not fall below {rate:?}");
            // Re-pricing at the achieved rate never costs more than was paid.
            let charged = fee_for(back, weight).unwrap();
            assert!(charged <= fee, "{charged} must not exceed {fee}");
        }
        assert!(vsize(weight) > 0);
    }

    mod fee_rate {
        use serde::Deserialize;

        use super::*;

        const FEE_RATE: &str = include_str!("../tests/vectors/fee_rate.json");

        #[derive(Deserialize)]
        struct File {
            fee: Vec<Fee>,
            rate: Vec<Rate>,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fee {
            sat_per_kwu: u64,
            weight_wu: u64,
            vsize: u64,
            #[serde(rename = "fee")]
            expected: u64,
        }

        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Rate {
            fee: u64,
            weight_wu: u64,
            sat_per_kwu: u64,
            sat_per_vb: String,
        }

        #[test]
        fn the_fee_and_the_achieved_rate_match_every_recorded_case() {
            let file: File = serde_json::from_str(FEE_RATE).unwrap();
            for case in &file.fee {
                let weight = Weight::from_wu(case.weight_wu);
                assert_eq!(vsize(weight), case.vsize, "{}", case.weight_wu);
                let fee = fee_for(FeeRate::from_sat_per_kwu(case.sat_per_kwu), weight).unwrap();
                assert_eq!(fee.base_units(), u128::from(case.expected), "{}", case.sat_per_kwu);
            }
            for case in &file.rate {
                let rate =
                    rate_for(Amount::from(case.fee), Weight::from_wu(case.weight_wu)).unwrap();
                assert_eq!(rate.sat_per_kwu(), case.sat_per_kwu, "{}", case.fee);
                assert_eq!(rate.format_sat_per_vb(), case.sat_per_vb, "{}", case.fee);
            }
        }
    }

    #[test]
    fn a_weight_rounds_up_to_whole_virtual_bytes_before_it_is_charged() {
        // 250 weight units round up to 63 virtual bytes, which pay 189 base units at
        // three per virtual byte. Charging the raw weight would pay 188.
        assert_eq!(
            fee_for(FeeRate::from_sat_per_kwu(750), Weight::from_wu(250)).unwrap(),
            Amount::from_base_units(189)
        );
    }

    /// The Litecoin file also records what the same buckets give under Bitcoin's
    /// target, to show why the target is a table field.
    #[test]
    fn the_litecoin_histogram_clears_at_its_target_and_differs_under_bitcoins() {
        let file: Value = serde_json::from_str(LITECOIN_FEE_RATES).unwrap();
        let histogram = file.pointer("/histogram").cloned().unwrap_or_default();
        let buckets: Vec<HistogramBucket> =
            serde_json::from_value(histogram.pointer("/buckets").cloned().unwrap_or_default())
                .unwrap();
        assert_eq!(normal_rate(&LITECOIN, &buckets), Some(kwu(number(&histogram, "rawKwu"))));
        assert_eq!(
            normal_rate(&BITCOIN, &buckets),
            Some(kwu(number(&histogram, "bitcoinRawKwu"))),
            "the same buckets against Bitcoin's three-block target"
        );
    }
}
