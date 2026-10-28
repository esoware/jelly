//! TRON resource charges.
//!
//! - Bandwidth must fit entirely in the staked pool or the free pool, or the whole cost
//!   burns TRX.
//! - Energy uses the account's allowance first and burns TRX for the rest.
//! - Activating an account can't use free bandwidth and also costs the system-contract
//!   fee.

use jelly_core::Amount;

use crate::config::CHAIN;
use crate::error::TronError;
use crate::params::ChainParams;
use crate::raw;
use crate::shape::Resources;

/// Margin over the measured energy, since the dynamic-energy surcharge can rise
/// before execution. Both `fee_limit` and the TRX a token transfer must hold
/// before sending include it.
const ENERGY_MARGIN_PERCENT: u64 = 20;

/// The lowest `fee_limit` a contract call carries, 100 TRX.
///
/// The limit only caps the burn, so a generous one costs nothing extra on a call
/// that succeeds.
const MIN_FEE_LIMIT_SUN: u64 = 100_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BandwidthSource {
    /// Covered by bandwidth from staked TRX.
    Staked,
    /// Covered by the free daily allowance.
    Free,
    /// Neither pool covers it on its own, so TRX is burned.
    Burned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct BandwidthCost {
    /// The serialized size plus the result allowance, times the activation rate
    /// when the transaction activates an account.
    pub(crate) points: u64,
    pub(crate) source: BandwidthSource,
    /// What the bandwidth alone burns. An activation's system-contract fee is
    /// [`TxCost::activation_fee_sun`].
    pub(crate) burn_sun: u64,
}

/// How a transaction of a given size is charged, at the resources and prices a
/// quote or build read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Charge {
    pub(crate) is_activation: bool,
    pub(crate) energy: Option<EnergyReading>,
    pub(crate) params: ChainParams,
    pub(crate) resources: Resources,
}

impl Charge {
    pub(crate) fn of(&self, raw_data: &[u8]) -> Result<TxCost, TronError> {
        let points = raw::bandwidth_points(raw_data)?;
        let bandwidth = if self.is_activation {
            activation_cost(points, &self.resources, &self.params)?
        } else {
            bandwidth_cost(points, &self.resources, &self.params)?
        };
        let energy = self
            .energy
            .map(|reading| energy_cost(reading, &self.resources, &self.params))
            .transpose()?;
        TxCost::new(bandwidth, energy, self.is_activation, &self.params)
    }
}

/// Bandwidth for a transaction that doesn't activate an account.
fn bandwidth_cost(
    points: u64,
    resources: &Resources,
    params: &ChainParams,
) -> Result<BandwidthCost, TronError> {
    let staked = resources.staked_bandwidth();
    let free = resources.free_bandwidth();
    let source = if staked >= points {
        BandwidthSource::Staked
    } else if free >= points {
        BandwidthSource::Free
    } else {
        BandwidthSource::Burned
    };
    let burn_sun = match source {
        BandwidthSource::Burned => points
            .checked_mul(params.bandwidth_price_sun)
            .ok_or(TronError::Overflow { what: "a bandwidth fee" })?,
        BandwidthSource::Staked | BandwidthSource::Free => 0,
    };
    Ok(BandwidthCost { points, source, burn_sun })
}

/// Bandwidth for a TRX transfer to an account that doesn't exist yet.
///
/// Free bandwidth can't pay for an activation, so it's staked bandwidth or the
/// flat `getCreateAccountFee`. The `getCreateNewAccountFeeInSystemContract`
/// charged either way isn't bandwidth, so [`TxCost::new`] adds it.
fn activation_cost(
    points: u64,
    resources: &Resources,
    params: &ChainParams,
) -> Result<BandwidthCost, TronError> {
    let rate = params.create_account_bandwidth_rate.max(1);
    let needed =
        points.checked_mul(rate).ok_or(TronError::Overflow { what: "a bandwidth cost" })?;
    let staked = resources.staked_bandwidth();
    let source = if staked >= needed { BandwidthSource::Staked } else { BandwidthSource::Burned };
    let burn_sun = match source {
        BandwidthSource::Burned => params.create_account_fee_sun,
        BandwidthSource::Staked | BandwidthSource::Free => 0,
    };
    Ok(BandwidthCost { points: needed, source, burn_sun })
}

/// What one contract call needs, as the node measured it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnergyReading {
    /// Total energy the call needs, surcharge included.
    pub(crate) needed: u64,
    /// The dynamic-energy surcharge inside `needed`. It rises with the contract's
    /// recent usage, so it can be higher at execution.
    pub(crate) penalty: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnergyCost {
    pub(crate) needed: u64,
    pub(crate) penalty: u64,
    pub(crate) available: u64,
    /// Energy that has to be paid for in TRX.
    pub(crate) shortfall: u64,
    pub(crate) burn_sun: u64,
}

/// Unlike bandwidth, the account's energy covers what it can and TRX pays for the
/// rest.
fn energy_cost(
    reading: EnergyReading,
    resources: &Resources,
    params: &ChainParams,
) -> Result<EnergyCost, TronError> {
    let available = resources.energy();
    let shortfall = reading.needed.saturating_sub(available);
    Ok(EnergyCost {
        needed: reading.needed,
        penalty: reading.penalty,
        available,
        shortfall,
        burn_sun: shortfall
            .checked_mul(params.energy_price_sun)
            .ok_or(TronError::Overflow { what: "an energy fee" })?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TxCost {
    pub(crate) bandwidth: BandwidthCost,
    pub(crate) energy: Option<EnergyCost>,
    /// True when this transfer creates the recipient's account.
    pub(crate) is_activation: bool,
    /// `getCreateNewAccountFeeInSystemContract` on an activation, and zero
    /// otherwise.
    pub(crate) activation_fee_sun: u64,
    /// TRX that actually leaves the balance, in sun.
    pub(crate) burn_sun: u64,
}

impl TxCost {
    fn new(
        bandwidth: BandwidthCost,
        energy: Option<EnergyCost>,
        is_activation: bool,
        params: &ChainParams,
    ) -> Result<Self, TronError> {
        let activation_fee_sun =
            if is_activation { params.create_account_contract_fee_sun } else { 0 };
        let burn_sun = bandwidth
            .burn_sun
            .checked_add(activation_fee_sun)
            .and_then(|sun| sun.checked_add(energy.map_or(0, |cost| cost.burn_sun)))
            .ok_or(TronError::Overflow { what: "a total fee" })?;
        Ok(Self { bandwidth, energy, is_activation, activation_fee_sun, burn_sun })
    }

    /// The burn if energy use rises by the full margin `fee_limit` allows. The
    /// balance has to hold this much so that only the fee limit can stop the call.
    pub(crate) fn padded_burn_sun(&self, params: &ChainParams) -> Result<u64, TronError> {
        let energy_sun = match self.energy {
            None => 0,
            Some(energy) => {
                let padded = u64::try_from(with_margin(u128::from(energy.needed))?)
                    .map_err(|_| TronError::Overflow { what: "a padded energy requirement" })?;
                padded
                    .saturating_sub(energy.available)
                    .checked_mul(params.energy_price_sun)
                    .ok_or(TronError::Overflow { what: "a padded energy fee" })?
            }
        };
        self.bandwidth
            .burn_sun
            .checked_add(self.activation_fee_sun)
            .and_then(|sun| sun.checked_add(energy_sun))
            .ok_or(TronError::Overflow { what: "a padded total fee" })
    }
}

/// The full measured requirement plus a margin, not just the shortfall.
///
/// A call can't use more energy than `fee_limit` divided by the price, even when
/// staked energy pays for it, and the surcharge can rise before execution. Unused
/// budget isn't charged.
///
/// Never below [`MIN_FEE_LIMIT_SUN`], so a surcharge jump at a maintenance period
/// can't run the call out of energy, and never above the network's maximum.
pub(crate) fn suggested_fee_limit(
    energy_needed: u64,
    params: &ChainParams,
) -> Result<u64, TronError> {
    let scaled = u128::from(energy_needed)
        .checked_mul(u128::from(params.energy_price_sun))
        .ok_or(TronError::Overflow { what: "a suggested fee limit" })
        .and_then(with_margin)?;
    let floored = scaled.max(u128::from(MIN_FEE_LIMIT_SUN));
    let capped = floored.min(u128::from(params.max_fee_limit_sun));
    Ok(u64::try_from(capped).unwrap_or(params.max_fee_limit_sun))
}

/// `value` and [`ENERGY_MARGIN_PERCENT`] more, rounded up.
fn with_margin(value: u128) -> Result<u128, TronError> {
    value
        .checked_mul(u128::from(100u64.saturating_add(ENERGY_MARGIN_PERCENT)))
        .map(|scaled| scaled.div_ceil(100))
        .ok_or(TronError::Overflow { what: "an energy margin" })
}

pub(crate) fn trx_text(sun: u64) -> String {
    CHAIN.format_amount(Amount::from(sun))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{priced_params, resources};

    #[test]
    fn bandwidth_is_all_or_nothing_per_pool() {
        let params = priced_params();
        // Staked covers it on its own.
        let cost = bandwidth_cost(265, &resources(0, 300, 0), &params).expect("a cost");
        assert_eq!(cost.source, BandwidthSource::Staked);
        assert_eq!(cost.burn_sun, 0);

        // Staked is short, so the free allowance covers it.
        let cost = bandwidth_cost(265, &resources(0, 200, 0), &params).expect("a cost");
        assert_eq!(cost.source, BandwidthSource::Free);
        assert_eq!(cost.burn_sun, 0);

        // Each pool holds 200 of the 265 points needed, so the whole cost burns
        // instead of being split across them.
        let cost = bandwidth_cost(265, &resources(400, 200, 0), &params).expect("a cost");
        assert_eq!(cost.source, BandwidthSource::Burned);
        assert_eq!(cost.burn_sun, 265_000);
    }

    #[test]
    fn activation_charges_the_creation_fee_and_never_the_free_allowance() {
        let params = priced_params();
        // Enough staked bandwidth, so only the one TRX creation fee.
        let cost = activation_cost(266, &resources(0, 300, 0), &params).expect("a cost");
        assert_eq!(cost.source, BandwidthSource::Staked);
        assert_eq!(cost.burn_sun, 0, "staked bandwidth covers the bandwidth");
        let total = TxCost::new(cost, None, true, &params).expect("a total");
        assert_eq!(total.activation_fee_sun, 1_000_000);
        assert_eq!(total.burn_sun, 1_000_000);

        // No stake, so the flat account-creation bandwidth fee applies too, even
        // though 600 free points would cover a plain transfer.
        let cost = activation_cost(266, &resources(0, 0, 0), &params).expect("a cost");
        assert_eq!(cost.source, BandwidthSource::Burned);
        assert_eq!(cost.burn_sun, 100_000);
        assert_eq!(cost.points, 266, "the rate is one on mainnet today");
        let total = TxCost::new(cost, None, true, &params).expect("a total");
        assert_eq!(total.burn_sun, 1_100_000);

        // The rate is a governance parameter. Above one, staked bandwidth has to
        // cover more.
        let dear = ChainParams { create_account_bandwidth_rate: 10, ..params };
        let cost = activation_cost(266, &resources(0, 300, 0), &dear).expect("a cost");
        assert_eq!(cost.points, 2_660);
        assert_eq!(cost.source, BandwidthSource::Burned);
    }

    #[test]
    fn energy_is_charged_proportionally_unlike_bandwidth() {
        let params = priced_params();
        let reading = EnergyReading { needed: 130_285, penalty: 100_635 };
        let cost = energy_cost(reading, &resources(0, 0, 19), &params).expect("a cost");
        assert_eq!(cost.available, 19);
        assert_eq!(cost.shortfall, 130_266);
        assert_eq!(cost.burn_sun, 13_026_600);

        let covered = energy_cost(reading, &resources(0, 0, 130_285), &params).expect("a cost");
        assert_eq!(covered.shortfall, 0);
        assert_eq!(covered.burn_sun, 0);
    }

    #[test]
    fn a_total_is_the_two_burns_added() {
        let params = priced_params();
        let bandwidth = bandwidth_cost(265, &resources(600, 0, 0), &params).expect("a cost");
        let energy =
            energy_cost(EnergyReading { needed: 100, penalty: 0 }, &resources(0, 0, 0), &params)
                .expect("a cost");
        let total = TxCost::new(bandwidth, Some(energy), false, &params).expect("a total");
        assert_eq!(total.burn_sun, 275_000);
        assert!(!total.is_activation);
        assert_eq!(total.activation_fee_sun, 0);
    }

    #[test]
    fn the_suggested_fee_limit_buys_the_whole_requirement_and_is_floored_and_capped() {
        let params = priced_params();
        // 1302850 energy at 100 sun, plus a twenty percent margin.
        assert_eq!(suggested_fee_limit(1_302_850, &params).expect("a limit"), 156_342_000);
        assert_eq!(
            suggested_fee_limit(130_285, &params).expect("a limit"),
            MIN_FEE_LIMIT_SUN,
            "an ordinary transfer gets the floor rather than its tight measurement"
        );
        assert_eq!(suggested_fee_limit(0, &params).expect("a limit"), MIN_FEE_LIMIT_SUN);
        assert_eq!(
            suggested_fee_limit(u64::MAX, &params).expect("a limit"),
            params.max_fee_limit_sun,
            "the protocol ceiling wins"
        );
    }

    #[test]
    fn the_padded_burn_pays_for_the_margin_the_fee_limit_allows_beyond_the_staked_energy() {
        let params = priced_params();
        let bandwidth = bandwidth_cost(345, &resources(600, 0, 0), &params).expect("a cost");
        let reading = EnergyReading { needed: 130_285, penalty: 100_635 };
        let energy = energy_cost(reading, &resources(0, 0, 19), &params).expect("a cost");
        let total = TxCost::new(bandwidth, Some(energy), false, &params).expect("a total");
        assert_eq!(total.burn_sun, 345_000 + 13_026_600, "the point burn");
        assert_eq!(
            total.padded_burn_sun(&params).expect("a padded burn"),
            345_000 + (156_342 - 19) * 100,
            "a fifth more energy than measured, less what the account holds"
        );

        let staked = energy_cost(reading, &resources(0, 0, 200_000), &params).expect("a cost");
        let covered = TxCost::new(bandwidth, Some(staked), false, &params).expect("a total");
        assert_eq!(covered.padded_burn_sun(&params).expect("a padded burn"), 345_000);

        let plain = TxCost::new(bandwidth, None, false, &params).expect("a total");
        assert_eq!(plain.padded_burn_sun(&params).expect("a padded burn"), plain.burn_sun);
    }

    #[test]
    fn sun_renders_as_trx_through_the_one_decimal_conversion() {
        assert_eq!(trx_text(0), "0 TRX");
        assert_eq!(trx_text(1), "0.000001 TRX");
        assert_eq!(trx_text(13_026_600), "13.0266 TRX");
        assert_eq!(trx_text(1_000_000), "1 TRX");
    }
}
