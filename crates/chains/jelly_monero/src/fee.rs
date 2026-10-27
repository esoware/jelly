use jelly_core::{Amount, FeeLevel, FeeQuote, FeeSelection, LevelFee};
use monero_interface::FeeRate;

use crate::config::MoneroConfig;
use crate::error::MoneroError;
use crate::params::RING_LEN;

/// The wallet2 priorities a fee level pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MoneroPriority {
    Unimportant,
    Normal,
    Elevated,
}

impl MoneroPriority {
    /// wallet2's own names, which the preview shows beside the fee. A level names a
    /// speed and this names the rate it paid, so the two differ when the normal level
    /// finds the chain quiet.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Unimportant => "Unimportant",
            Self::Normal => "Normal",
            Self::Elevated => "Elevated",
        }
    }
}

/// The most a fee may pay per unit of weight, whether a node quotes it or the user
/// chooses it.
///
/// It must be at least the highest tier a level pays, which is up to 25 times the base
/// rate, because only a rate above it is refused.
pub(crate) const MAX_PICONERO_PER_WEIGHT: u64 = 100_000_000;

/// What a request asks its fee to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chosen {
    Level(FeeLevel),
    /// A custom rate in piconero per unit of weight.
    Rate(u64),
}

/// One node's fee tiers from one `get_fee_estimate` call, rather than a call per tier
/// against a host that rate-limits, and the priority the normal level pays.
#[derive(Debug, Clone)]
pub struct FeeLadder {
    rates: [FeeRate; 3],
    mask: u64,
    normal: MoneroPriority,
}

impl FeeLadder {
    /// The unimportant, normal, and elevated piconero-per-weight rates, with the
    /// quantisation mask monerod rounds fees up to. `None` when the node quoted a zero
    /// rate or a zero mask, neither of which is a usable fee.
    ///
    /// The normal level pays the normal priority until `FeeLadder::with_normal`
    /// says how full the chain is.
    pub fn new(piconero_per_weight: [u64; 3], mask: u64) -> Option<Self> {
        let [unimportant, normal, elevated] = piconero_per_weight;
        let rates = [
            FeeRate::new(unimportant, mask)?,
            FeeRate::new(normal, mask)?,
            FeeRate::new(elevated, mask)?,
        ];
        Some(Self { rates, mask, normal: MoneroPriority::Normal })
    }

    /// The priority and rate `chosen` pays. A custom rate pays no priority, and has to
    /// clear the slow level's rate, which is the least monerod relays.
    pub(crate) fn resolve(
        &self,
        chosen: Chosen,
    ) -> Result<(Option<MoneroPriority>, FeeRate), MoneroError> {
        match chosen {
            Chosen::Level(level) => {
                let priority = self.priority(level);
                Ok((Some(priority), self.rate(priority)))
            }
            Chosen::Rate(per_weight) => {
                let minimum = self.rate(MoneroPriority::Unimportant).per_weight();
                if per_weight < minimum {
                    return Err(MoneroError::FeeRateBelowMinimum { rate: per_weight, minimum });
                }
                if per_weight > MAX_PICONERO_PER_WEIGHT {
                    return Err(MoneroError::FeeRateAboveMaximum {
                        maximum: MAX_PICONERO_PER_WEIGHT,
                    });
                }
                let rate = FeeRate::new(per_weight, self.mask)
                    .ok_or(MoneroError::Internal { context: "a custom rate is zero" })?;
                Ok((None, rate))
            }
        }
    }

    #[must_use]
    pub(crate) const fn with_normal(mut self, priority: MoneroPriority) -> Self {
        self.normal = priority;
        self
    }

    /// The slow level pays the unimportant rate and the fast level the elevated one.
    /// The normal level pays what wallet2's default priority would, which on a quiet
    /// chain is also the unimportant rate.
    pub(crate) const fn priority(&self, level: FeeLevel) -> MoneroPriority {
        match level {
            FeeLevel::Slow => MoneroPriority::Unimportant,
            FeeLevel::Normal => self.normal,
            FeeLevel::Fast => MoneroPriority::Elevated,
        }
    }

    pub(crate) const fn rate(&self, priority: MoneroPriority) -> FeeRate {
        let [unimportant, normal, elevated] = self.rates;
        match priority {
            MoneroPriority::Unimportant => unimportant,
            MoneroPriority::Normal => normal,
            MoneroPriority::Elevated => elevated,
        }
    }

    pub(crate) const fn rates(&self) -> &[FeeRate; 3] {
        &self.rates
    }
}

/// Blocks wallet2 reads to judge how full the chain is.
pub(crate) const RECENT_BLOCKS: u8 = 10;

/// Percent of the full reward zone above which recent blocks count as full.
const FULL_PERCENT: u128 = 80;

/// Every transaction this adapter builds pays one recipient and returns change.
pub(crate) const PAYMENT_AND_CHANGE: u64 = 2;
const _: () =
    assert!(PAYMENT_AND_CHANGE == 2, "The weight estimate is wallet2's for exactly two outputs.");

/// The `extra` field of every transaction this adapter builds.
///
/// That is one transaction public key, 1 + 32 for its tag and the key, and an
/// encrypted payment id, 1 + 1 + 1 + 8 for the nonce tag, its length, the payment
/// id tag, and the id.
///
/// monero-wallet adds no additional keys to a two-output transaction whose
/// change it can view, so paying a subaddress makes the one key `rB` and leaves
/// the extra the same size as paying a standard address does.
const EXTRA_SIZE: u64 = 44;

/// The level or custom rate a request asked for.
///
/// Another chain's custom fee is refused rather than defaulted, because it means the
/// interface and the adapter disagree about which chain is being paid.
pub(crate) fn chosen_fee(selection: &FeeSelection) -> Result<Chosen, MoneroError> {
    match *selection {
        FeeSelection::Level { level } => Ok(Chosen::Level(level)),
        FeeSelection::Rate { ref rate } => {
            rate.trim().parse::<u64>().map(Chosen::Rate).map_err(|_| MoneroError::FeeRateUnreadable)
        }
        FeeSelection::Evm { .. } | FeeSelection::Solana { .. } | FeeSelection::Total { .. } => {
            Err(MoneroError::ForeignFeeSelection)
        }
    }
}

/// wallet2's `adjust_priority`, which is what its default priority does, and so
/// what the normal level pays.
///
/// Normal when the pool holds a full reward zone, half the block weight limit, of
/// transactions, or when the last ten blocks fill more than 80 percent of it, and
/// unimportant otherwise. wallet2 counts only pool transactions paying at least
/// the unimportant rate, which monerod answers as a binary blob even over JSON,
/// so `pool_weight` is the whole pool's weight. That overcounts only transactions
/// paying less than the cheapest priority, which are rare. Unlike wallet2, which
/// falls back to unimportant, anything this cannot judge is normal, because a
/// Monero fee cannot be bumped.
pub(crate) fn automatic_priority(
    pool_weight: u64,
    block_weights: &[u64],
    block_weight_limit: u64,
) -> MoneroPriority {
    let full_reward_zone = u128::from(block_weight_limit.div_euclid(2));
    if block_weights.len() != usize::from(RECENT_BLOCKS)
        || full_reward_zone == 0
        || u128::from(pool_weight) >= full_reward_zone
    {
        return MoneroPriority::Normal;
    }
    let filled =
        block_weights.iter().fold(0u128, |sum, weight| sum.saturating_add(u128::from(*weight)));
    let capacity = full_reward_zone.saturating_mul(u128::from(RECENT_BLOCKS));
    let percent = filled.saturating_mul(100).checked_div(capacity).unwrap_or(u128::MAX);
    if percent > FULL_PERCENT { MoneroPriority::Normal } else { MoneroPriority::Unimportant }
}

/// wallet2's `estimate_rct_tx_size` for the one shape this adapter builds.
///
/// That shape is CLSAG with Bulletproofs+ and view tags, a ring of [`RING_LEN`],
/// one payment and its change, and an [`EXTRA_SIZE`] extra field. wallet2's
/// `estimate_tx_weight` adds a Bulletproof clawback only past two outputs, so for
/// this shape the weight is the size.
///
/// From `src/wallet/wallet2.cpp`. This estimate never sets a fee. The fee a
/// transaction commits to is always `SignableTransaction::necessary_fee`, from
/// the real weight.
pub(crate) fn estimate_weight(n_inputs: usize) -> u64 {
    let inputs = u64::try_from(n_inputs).unwrap_or(u64::MAX);
    let outputs = PAYMENT_AND_CHANGE;
    let ring = u64::from(RING_LEN);
    // Prefix with version and unlock time.
    let mut size = 7u64;
    // Inputs with amount, one varint per ring member, and key image.
    size = size.saturating_add(
        inputs.saturating_mul(7u64.saturating_add(ring.saturating_mul(2)).saturating_add(32)),
    );
    // Outputs with amount, key, and view tag.
    size = size.saturating_add(outputs.saturating_mul(39));
    size = size.saturating_add(EXTRA_SIZE);
    // RingCT type byte.
    size = size.saturating_add(1);
    // Bulletproof+. Two outputs are already a power of two, so the padding
    // exponent is one.
    size = size.saturating_add(
        (2u64.saturating_mul(6u64.saturating_add(1)).saturating_add(6))
            .saturating_mul(32)
            .saturating_add(3),
    );
    // CLSAG per input.
    size =
        size.saturating_add(inputs.saturating_mul(32u64.saturating_mul(ring).saturating_add(64)));
    // Pseudo-outs, ecdhInfo, outPk and the fee.
    size = size.saturating_add(inputs.saturating_mul(32));
    size = size.saturating_add(outputs.saturating_mul(8));
    size = size.saturating_add(outputs.saturating_mul(32));
    size.saturating_add(4)
}

/// The fee a payment spending `n_inputs` outputs would pay at this rate,
/// quantised the way the network quantises it.
pub(crate) fn estimate_fee(rate: FeeRate, n_inputs: usize) -> Amount {
    Amount::from_base_units(rate.calculate_fee_from_weight(estimate_weight(n_inputs)))
}

/// The fewest leading outputs whose total covers the payment and the fee for
/// spending exactly them, or all of them for a sweep.
///
/// The caller sorts the amounts largest first. The build starts from this count
/// and grows it only if the exact fee disagrees, so a quote and the build that
/// follows count the same inputs.
pub(crate) fn inputs_for(
    amounts_piconero: impl IntoIterator<Item = u64>,
    target_piconero: Option<u64>,
    rate: FeeRate,
) -> usize {
    let Some(target_piconero) = target_piconero else {
        return amounts_piconero.into_iter().count().max(1);
    };
    let mut held_piconero: u128 = 0;
    let mut count = 0usize;
    for amount in amounts_piconero {
        count = count.saturating_add(1);
        held_piconero = held_piconero.saturating_add(u128::from(amount));
        let projected = estimate_fee(rate, count);
        if held_piconero >= u128::from(target_piconero).saturating_add(projected.base_units()) {
            break;
        }
    }
    count.max(1)
}

/// What a transaction of the shape this request implies pays at `rate`, beside what
/// it would pay at every level.
///
/// Monero prices per byte of weight, so the exact fee is fixed only once the
/// transaction is built. The input count is the one estimated dimension, because
/// a dearer rate can need one more input to pay for itself. A sweep's amount
/// inherits the estimate.
pub(crate) fn quote(
    config: &MoneroConfig,
    ladder: &FeeLadder,
    rate: FeeRate,
    amounts_piconero: &[u64],
    target_piconero: Option<u64>,
) -> FeeQuote {
    let input_count = |rate: FeeRate| {
        inputs_for(amounts_piconero.iter().copied(), target_piconero, rate)
            .clamp(1, config.inputs())
    };
    let fee_at = |rate: FeeRate| estimate_fee(rate, input_count(rate));
    let fee = fee_at(rate);
    let max_amount = if target_piconero.is_none() {
        let held_piconero = amounts_piconero
            .iter()
            .take(input_count(rate))
            .fold(0u128, |sum, &amount| sum.saturating_add(u128::from(amount)));
        Amount::from_base_units(held_piconero).checked_sub(fee)
    } else {
        None
    };
    let levels = FeeLevel::ALL
        .into_iter()
        .map(|level| LevelFee { level, fee: fee_at(ladder.rate(ladder.priority(level))) })
        .collect();
    let custom = FeeSelection::Rate { rate: rate.per_weight().to_string() };
    FeeQuote { fee, custom: Some(custom), max_amount, levels }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Deserialize)]
    struct Weights {
        cases: Vec<WeightCase>,
    }

    #[derive(Deserialize)]
    struct WeightCase {
        inputs: usize,
        weight: u64,
    }

    #[test]
    fn every_input_adds_weight() {
        let one = estimate_weight(1);
        assert!(estimate_weight(2) > one);
        assert!(estimate_weight(4) > estimate_weight(2));
    }

    #[test]
    fn a_fee_is_quantised_up_to_the_mask() {
        let rate = FeeRate::new(20, 10_000).expect("a usable rate");
        let weight = estimate_weight(1);
        let fee = estimate_fee(rate, 1);
        let expected =
            (u128::from(weight).saturating_mul(20)).div_ceil(10_000).saturating_mul(10_000);
        assert_eq!(fee, Amount::from_base_units(expected));
        assert_eq!(fee.base_units().checked_rem(10_000), Some(0));
    }

    #[test]
    fn a_level_or_a_whole_rate_is_read_and_another_chains_fee_is_refused() {
        for level in FeeLevel::ALL {
            assert_eq!(
                chosen_fee(&FeeSelection::Level { level }).expect("a level"),
                Chosen::Level(level)
            );
        }
        let rate = |text: &str| chosen_fee(&FeeSelection::Rate { rate: text.to_owned() });
        assert_eq!(rate(" 80000 ").expect("a whole rate"), Chosen::Rate(80_000));
        for unreadable in ["1.5", "-3", "fast", ""] {
            assert!(
                matches!(rate(unreadable), Err(MoneroError::FeeRateUnreadable)),
                "{unreadable}"
            );
        }
        assert!(matches!(
            chosen_fee(&FeeSelection::Solana { priority_fee_lamports: "3".to_owned() }),
            Err(MoneroError::ForeignFeeSelection)
        ));
    }

    #[test]
    fn a_custom_rate_pays_no_priority_and_has_to_clear_the_slow_rate() {
        let ladder = FeeLadder::new([20_000, 80_000, 320_000], 10_000).expect("a usable ladder");
        assert_eq!(
            ladder.resolve(Chosen::Level(FeeLevel::Fast)).expect("a level"),
            (Some(MoneroPriority::Elevated), ladder.rate(MoneroPriority::Elevated))
        );
        let (priority, rate) = ladder.resolve(Chosen::Rate(50_000)).expect("a custom rate");
        assert_eq!(priority, None);
        assert_eq!(rate, FeeRate::new(50_000, 10_000).expect("a rate"), "the ladder's mask");
        assert!(ladder.resolve(Chosen::Rate(20_000)).is_ok(), "the slow rate itself");
        assert!(matches!(
            ladder.resolve(Chosen::Rate(19_999)),
            Err(MoneroError::FeeRateBelowMinimum { rate: 19_999, minimum: 20_000 })
        ));
        assert!(matches!(
            ladder.resolve(Chosen::Rate(MAX_PICONERO_PER_WEIGHT.saturating_add(1))),
            Err(MoneroError::FeeRateAboveMaximum { .. })
        ));
    }

    #[test]
    fn the_slow_and_fast_levels_are_fixed_and_the_normal_one_follows_the_chain() {
        let ladder = FeeLadder::new([1, 4, 16], 1).expect("a usable ladder");
        assert_eq!(ladder.priority(FeeLevel::Slow), MoneroPriority::Unimportant);
        assert_eq!(ladder.priority(FeeLevel::Normal), MoneroPriority::Normal);
        assert_eq!(ladder.priority(FeeLevel::Fast), MoneroPriority::Elevated);

        let quiet = ladder.with_normal(MoneroPriority::Unimportant);
        assert_eq!(quiet.priority(FeeLevel::Normal), MoneroPriority::Unimportant);
        assert_eq!(quiet.priority(FeeLevel::Fast), MoneroPriority::Elevated);
    }

    #[test]
    fn a_quote_prices_every_level_and_names_its_rate_as_a_custom_fee() {
        let ladder = FeeLadder::new([20_000, 80_000, 320_000], 10_000).expect("a usable ladder");
        let config = MoneroConfig::default();
        let fast = ladder.rate(MoneroPriority::Elevated);
        let quote = quote(&config, &ladder, fast, &[10_000_000_000], Some(1_000));
        assert_eq!(quote.custom, Some(FeeSelection::Rate { rate: String::from("320000") }));
        let levels: Vec<FeeLevel> = quote.levels.iter().map(|row| row.level).collect();
        assert_eq!(levels, FeeLevel::ALL);
        assert_eq!(quote.levels.last().map(|row| row.fee), Some(quote.fee));
        assert!(quote.levels.windows(2).all(|pair| match pair {
            [slower, faster] => slower.fee < faster.fee,
            _ => false,
        }));
    }

    /// wallet2 pays the lowest priority unless the pool holds a full reward zone,
    /// which is half the weight limit, or recent blocks fill more than 80 percent
    /// of it.
    #[test]
    fn the_automatic_priority_is_unimportant_until_the_pool_or_recent_blocks_are_full() {
        // A 600000-byte limit is a 300000-byte zone, whose 80 percent is 240000.
        let limit = 600_000u64;
        let recent = usize::from(RECENT_BLOCKS);
        let quiet = vec![20_000; recent];
        assert_eq!(
            automatic_priority(0, &vec![240_000; recent], limit),
            MoneroPriority::Unimportant
        );
        assert_eq!(automatic_priority(0, &vec![270_000; recent], limit), MoneroPriority::Normal);
        assert_eq!(automatic_priority(0, &quiet, limit), MoneroPriority::Unimportant);
        assert_eq!(automatic_priority(299_999, &quiet, limit), MoneroPriority::Unimportant);
        assert_eq!(
            automatic_priority(300_000, &quiet, limit),
            MoneroPriority::Normal,
            "a pool holding a whole reward zone is a backlog of one block"
        );
        assert_eq!(
            automatic_priority(0, &[20_000; 3], limit),
            MoneroPriority::Normal,
            "a short answer cannot be judged"
        );
        assert_eq!(automatic_priority(0, &quiet, 0), MoneroPriority::Normal);
    }

    #[test]
    fn the_input_count_stops_at_the_first_coin_that_covers_the_payment_and_its_fee() {
        let rate = FeeRate::new(1, 1).expect("a usable rate");
        let one_input = estimate_weight(1);
        let coins = [one_input.saturating_add(500), 10_000, 10_000];

        assert_eq!(
            inputs_for(coins, Some(400), rate),
            1,
            "the first coin covers the payment and the fee"
        );
        assert_eq!(inputs_for(coins, Some(600), rate), 2, "and one piconero more does not");
        assert_eq!(inputs_for(coins, None, rate), 3, "a sweep spends every coin it was given");
        assert_eq!(
            inputs_for([], Some(1), rate),
            1,
            "never zero: an empty set is still one transaction's worth of estimate"
        );
        assert_eq!(
            inputs_for(coins, Some(u64::MAX), rate),
            3,
            "a payment nothing covers takes everything there is"
        );
    }

    #[test]
    fn the_estimator_matches_the_pinned_weights() {
        let vectors: Weights = serde_json::from_str(include_str!("../tests/vectors/weight.json"))
            .expect("the vector file parses");
        assert!(!vectors.cases.is_empty());
        for case in &vectors.cases {
            assert_eq!(estimate_weight(case.inputs), case.weight, "{} in", case.inputs);
        }
    }
}
