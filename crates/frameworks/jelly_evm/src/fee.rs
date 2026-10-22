use jelly_core::{FeeLevel, FeeSelection, TxRequest};

use crate::config::EvmConfig;
use crate::error::EvmError;
use crate::params::EvmParams;
use crate::rpc::FeeHistory;
use crate::units;

/// A fee cap and priority fee, in wei per gas.
///
/// Validated at construction, because EIP-1559 makes a transaction whose tip
/// exceeds its fee cap invalid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeePair {
    max_fee_per_gas: u128,
    max_priority_fee_per_gas: u128,
}

impl FeePair {
    pub(crate) const ZERO: Self = Self { max_fee_per_gas: 0, max_priority_fee_per_gas: 0 };

    pub(crate) const fn new(
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> Result<Self, EvmError> {
        if max_priority_fee_per_gas > max_fee_per_gas {
            return Err(EvmError::TipAboveCap {
                tip_wei_per_gas: max_priority_fee_per_gas,
                cap_wei_per_gas: max_fee_per_gas,
            });
        }
        Ok(Self { max_fee_per_gas, max_priority_fee_per_gas })
    }

    pub(crate) const fn max_fee_per_gas(self) -> u128 {
        self.max_fee_per_gas
    }

    pub(crate) const fn max_priority_fee_per_gas(self) -> u128 {
        self.max_priority_fee_per_gas
    }

    /// The most this pair can cost at `gas_limit`. The node reserves this much
    /// until execution, so the balance must cover it and a send-max subtracts it.
    pub(crate) fn max_cost_wei(self, gas_limit: u64) -> Result<u128, EvmError> {
        units::cost("max fee", self.max_fee_per_gas, gas_limit)
    }

    /// The cost if the base fee does not move, at `min(cap, base + tip)` per gas.
    pub(crate) fn expected_cost_wei(
        self,
        gas_limit: u64,
        base_fee_per_gas: u128,
    ) -> Result<u128, EvmError> {
        let per_gas = base_fee_per_gas
            .checked_add(self.max_priority_fee_per_gas)
            .ok_or(EvmError::FeeOverflow { field: "expected fee" })?
            .min(self.max_fee_per_gas);
        units::cost("expected fee", per_gas, gas_limit)
    }

    /// The lowest pair a node takes as a replacement for this one.
    ///
    /// Geth and its forks require both halves at least 10 percent higher. Rounded
    /// up, because one wei short is a refusal.
    pub(crate) fn replacement_floor(self) -> Result<Self, EvmError> {
        Self::new(
            units::scale_pct_up("max fee per gas", self.max_fee_per_gas, 110)?,
            units::scale_pct_up("max priority fee per gas", self.max_priority_fee_per_gas, 110)?,
        )
    }

    /// Raises each half on its own, because a node compares them separately and a
    /// cap that clears the floor may still carry a low tip.
    pub(crate) fn raised_to(self, floor: Self) -> Result<Self, EvmError> {
        Self::new(
            self.max_fee_per_gas.max(floor.max_fee_per_gas),
            self.max_priority_fee_per_gas.max(floor.max_priority_fee_per_gas),
        )
    }

    pub(crate) const fn clears(self, floor: Self) -> bool {
        self.max_fee_per_gas >= floor.max_fee_per_gas
            && self.max_priority_fee_per_gas >= floor.max_priority_fee_per_gas
    }

    /// The custom fee a user picking this pair and `gas_limit` by hand would have
    /// sent.
    pub(crate) fn custom(self, gas_limit: u64) -> FeeSelection {
        FeeSelection::Evm {
            gas_limit: Some(gas_limit.to_string()),
            max_fee_per_gas: Some(self.max_fee_per_gas.to_string()),
            max_priority_fee_per_gas: Some(self.max_priority_fee_per_gas.to_string()),
        }
    }
}

/// How one fee level reads the market, after `MetaMask`'s `eth_feeHistory` estimator.
///
/// The tip is the median across the sampled blocks of the reward at `percentile`,
/// scaled by `tip_pct`. The cap is the next block's base fee scaled by
/// `base_fee_pct`, plus that tip. `MetaMask`'s floors of one to two gwei suit only
/// Ethereum, so each network's own floor and ceiling hold the tip instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LevelTuning {
    level: FeeLevel,
    percentile: u8,
    tip_pct: u64,
    base_fee_pct: u64,
}

/// In [`FeeLevel::ALL`] order.
///
/// That is also the order `eth_feeHistory` answers the percentiles in. The
/// percentiles rise with the level and the reward columns are sampled from the same
/// blocks, so a faster level never comes out cheaper than a slower one.
const LEVELS: [LevelTuning; 3] = [
    LevelTuning { level: FeeLevel::Slow, percentile: 10, tip_pct: 94, base_fee_pct: 110 },
    LevelTuning { level: FeeLevel::Normal, percentile: 20, tip_pct: 97, base_fee_pct: 120 },
    LevelTuning { level: FeeLevel::Fast, percentile: 30, tip_pct: 98, base_fee_pct: 125 },
];

/// The reward percentiles one `eth_feeHistory` read asks for, one per level.
pub(crate) const REWARD_PERCENTILES: [u8; 3] =
    [LEVELS[0].percentile, LEVELS[1].percentile, LEVELS[2].percentile];

/// The next block's base fee and the pair each fee level pays, from one
/// `eth_feeHistory` read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeeMarket {
    base_fee_per_gas: u128,
    /// In the order of [`LEVELS`].
    levels: [FeePair; 3],
}

impl FeeMarket {
    /// A tip is floored at the network minimum, which on BSC is what gets a
    /// transaction included, and held under the network's ceiling where it has one.
    ///
    /// Tolerates what public nodes really return, such as a missing `reward`
    /// matrix, zero rows from empty blocks, and a `baseFeePerGas` array without
    /// the trailing prediction.
    pub(crate) fn from_history(history: &FeeHistory, params: &EvmParams) -> Result<Self, EvmError> {
        let base_fee_per_gas = history.predicted_base_fee()?;
        let mut levels = [FeePair::ZERO; 3];
        for (column, (slot, tuning)) in levels.iter_mut().zip(LEVELS).enumerate() {
            let sampled =
                units::scale_pct("priority fee", median(&history.column(column)?), tuning.tip_pct)?
                    .max(params.min_priority_fee_wei);
            let tip = params.max_priority_fee_wei.map_or(sampled, |ceiling| sampled.min(ceiling));
            let cap =
                units::scale_pct_up("max fee per gas", base_fee_per_gas, tuning.base_fee_pct)?
                    .checked_add(tip)
                    .ok_or(EvmError::FeeOverflow { field: "max fee per gas" })?;
            *slot = FeePair::new(cap, tip)?;
        }
        Ok(Self { base_fee_per_gas, levels })
    }

    pub(crate) const fn base_fee_per_gas(self) -> u128 {
        self.base_fee_per_gas
    }

    pub(crate) const fn level(self, level: FeeLevel) -> FeePair {
        let [slow, normal, fast] = self.levels;
        match level {
            FeeLevel::Slow => slow,
            FeeLevel::Normal => normal,
            FeeLevel::Fast => fast,
        }
    }

    /// Every level's pair, slowest first.
    pub(crate) fn levels(self) -> impl Iterator<Item = (FeeLevel, FeePair)> {
        FeeLevel::ALL.into_iter().zip(self.levels)
    }
}

/// The even case adds half the gap to the lower sample rather than halving the
/// sum, which could overflow.
fn median(values: &[u128]) -> u128 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let middle = sorted.len().checked_div(2).unwrap_or(0);
    if sorted.len().checked_rem(2) == Some(1) {
        return sorted.get(middle).copied().unwrap_or(0);
    }
    let lower = middle.checked_sub(1).and_then(|index| sorted.get(index).copied()).unwrap_or(0);
    let upper = sorted.get(middle).copied().unwrap_or(0);
    let half_gap = upper.saturating_sub(lower).checked_div(2).unwrap_or(0);
    lower.checked_add(half_gap).unwrap_or(upper)
}

/// Shown in the preview, because a limit set by hand was never checked against the
/// call and an out-of-gas execution still burns the whole limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GasSource {
    Estimated,
    UserOverride,
}

impl GasSource {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Estimated => "Estimated by the node",
            Self::UserOverride => "Set by hand",
        }
    }
}

/// The intrinsic gas of a transaction that carries no calldata and runs no code.
const TRANSFER_GAS: u64 = 21_000;

/// The EIP-7825 per-transaction gas cap, live on Ethereum mainnet since Fusaka.
const MAX_TX_GAS: u64 = 16_777_216;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GasLimit {
    limit: u64,
    /// What the transaction is expected to use, which is the node's unpadded
    /// estimate. Unused gas is refunded, so this rather than the limit prices the
    /// fee a user should expect.
    expected: u64,
    source: GasSource,
}

impl GasLimit {
    /// EIP-7825 caps one transaction at 2^24 gas, so a limit above it is refused
    /// here rather than at the node.
    pub(crate) const fn new(limit: u64, source: GasSource) -> Result<Self, EvmError> {
        if limit > MAX_TX_GAS {
            return Err(EvmError::GasAboveCap { gas_limit: limit, cap_gas: MAX_TX_GAS });
        }
        Ok(Self { limit, expected: limit, source })
    }

    /// An estimate of exactly the intrinsic gas is a transfer that runs no code,
    /// which always uses exactly that, so it is not padded. `MetaMask` keeps such a
    /// transfer at 21000 too.
    pub(crate) fn estimated(estimate: u64, config: &EvmConfig) -> Result<Self, EvmError> {
        let gas = Self::new(estimate, GasSource::Estimated)?;
        let padding_pct = if estimate <= TRANSFER_GAS { 0 } else { config.gas_padding_pct };
        let padded = units::scale_pct(
            "gas limit",
            u128::from(estimate),
            100u64.saturating_add(padding_pct),
        )?;
        let limit = u64::try_from(padded.min(u128::from(MAX_TX_GAS))).unwrap_or(MAX_TX_GAS);
        Ok(Self { limit, ..gas })
    }

    pub(crate) const fn limit(self) -> u64 {
        self.limit
    }

    pub(crate) const fn expected(self) -> u64 {
        self.expected
    }

    pub(crate) const fn source(self) -> GasSource {
        self.source
    }
}

/// What a request pins by hand, read once, so a malformed value is refused before
/// any of it reaches a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Overrides {
    /// The level whose pair every half the request leaves automatic takes.
    pub(crate) level: FeeLevel,
    pub(crate) gas_limit: Option<u64>,
    pub(crate) max_fee_per_gas: Option<u128>,
    pub(crate) max_priority_fee_per_gas: Option<u128>,
    pub(crate) nonce: Option<u64>,
    pub(crate) calldata: Option<Vec<u8>>,
}

impl Overrides {
    /// The price of `request` with its nonce and calldata. Another chain's selection
    /// is refused rather than silently ignored.
    pub(crate) fn of(request: &TxRequest) -> Result<Self, EvmError> {
        let calldata =
            request.data.as_deref().map(|text| units::parse_hex("calldata", text)).transpose()?;
        Ok(Self { nonce: request.nonce, calldata, ..Self::price_of(&request.fee)? })
    }

    /// Only the level, the price and the gas limit of `selection`, for a replacement,
    /// whose nonce and calldata come from the send it replaces.
    pub(crate) fn price_of(selection: &FeeSelection) -> Result<Self, EvmError> {
        match *selection {
            FeeSelection::Level { level } => Ok(Self { level, ..Self::AUTOMATIC }),
            FeeSelection::Evm {
                ref gas_limit,
                ref max_fee_per_gas,
                ref max_priority_fee_per_gas,
            } => {
                let max_fee_per_gas = max_fee_per_gas
                    .as_deref()
                    .map(|text| units::parse_integer("max fee per gas", text))
                    .transpose()?;
                let max_priority_fee_per_gas = max_priority_fee_per_gas
                    .as_deref()
                    .map(|text| units::parse_integer("max priority fee per gas", text))
                    .transpose()?;
                let gas_limit = gas_limit
                    .as_deref()
                    .map(|text| units::parse_gas("gas limit", text))
                    .transpose()?;
                Ok(Self { gas_limit, max_fee_per_gas, max_priority_fee_per_gas, ..Self::AUTOMATIC })
            }
            FeeSelection::Rate { .. }
            | FeeSelection::Solana { .. }
            | FeeSelection::Total { .. } => Err(EvmError::ForeignFeeSelection),
        }
    }

    /// Nothing pinned, at the normal level.
    pub(crate) const AUTOMATIC: Self = Self {
        level: FeeLevel::Normal,
        gas_limit: None,
        max_fee_per_gas: None,
        max_priority_fee_per_gas: None,
        nonce: None,
        calldata: None,
    };

    pub(crate) const fn pins_the_fee(&self) -> bool {
        self.max_fee_per_gas.is_some() && self.max_priority_fee_per_gas.is_some()
    }

    /// A half the request leaves automatic is the market's at the request's level,
    /// raised to `floor` when the build reuses a nonce slot, so an automatic price
    /// always clears what the slot's other transactions pay.
    pub(crate) fn pair(
        &self,
        automatic: FeePair,
        floor: Option<FeePair>,
    ) -> Result<FeePair, EvmError> {
        let default = match floor {
            Some(floor) => automatic.raised_to(floor)?,
            None => automatic,
        };
        FeePair::new(
            self.max_fee_per_gas.unwrap_or_else(|| default.max_fee_per_gas()),
            self.max_priority_fee_per_gas.unwrap_or_else(|| default.max_priority_fee_per_gas()),
        )
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::{Amount, AssetId, ChainId, test_util::TxRequestBuilder};

    use super::*;
    use crate::test_util::{ARBITRUM, AVALANCHE, BASE, BSC, ETHEREUM, POLYGON};

    const GWEI: u128 = 1_000_000_000;

    fn history(body: &str) -> FeeHistory {
        serde_json::from_str(body).expect("the recorded answer decodes")
    }

    #[test]
    fn a_pair_refuses_a_tip_above_its_cap() {
        FeePair::new(30 * GWEI, GWEI).unwrap();
        assert!(matches!(
            FeePair::new(GWEI, 30 * GWEI),
            Err(EvmError::TipAboveCap { tip_wei_per_gas, cap_wei_per_gas })
                if tip_wei_per_gas == 30 * GWEI && cap_wei_per_gas == GWEI
        ));
        let refused = FeePair::new(GWEI, 30 * GWEI).expect_err("the tip is above the cap");
        assert_eq!(
            refused.to_string(),
            "The priority fee of 30000000000 wei per gas exceeds the maximum fee of 1000000000 \
             wei per gas."
        );
    }

    #[test]
    fn the_worst_case_uses_the_cap_and_the_estimate_uses_the_base_fee() {
        let pair = FeePair::new(30 * GWEI, 2 * GWEI).expect("a pair");
        assert_eq!(pair.max_cost_wei(21_000).expect("cost"), 30 * GWEI * 21_000);
        assert_eq!(pair.expected_cost_wei(21_000, 10 * GWEI).expect("cost"), 12 * GWEI * 21_000);
        assert_eq!(
            pair.expected_cost_wei(21_000, 100 * GWEI).expect("cost"),
            30 * GWEI * 21_000,
            "the estimate never rises above the cap"
        );
    }

    #[test]
    fn a_replacement_floor_is_a_tenth_higher_on_both_halves_and_rounds_up() {
        let original = FeePair::new(101, 11).expect("a pair");
        let floor = original.replacement_floor().expect("a floor");
        assert_eq!(floor.max_fee_per_gas(), 112, "101 * 1.1 rounded up");
        assert_eq!(floor.max_priority_fee_per_gas(), 13, "11 * 1.1 rounded up");
        assert!(floor.clears(floor));
        assert!(!original.clears(floor));
        assert!(FeePair::new(112, 13).expect("a pair").clears(floor));
        assert!(!FeePair::new(112, 12).expect("a pair").clears(floor), "both halves must clear");
    }

    /// Each block answers the tenth, twentieth and thirtieth percentile, and the
    /// next base fee is twelve gwei.
    const RISING: &str = r#"{
        "baseFeePerGas":["0x2540be400","0x28fa6ae00","0x2cb417800","0x2cb417800"],
        "reward":[
            ["0x3b9aca00","0x77359400","0xb2d05e00"],
            ["0x3b9aca00","0x77359400","0xee6b2800"],
            ["0x77359400","0xb2d05e00","0x12a05f200"]
        ]
    }"#;

    #[test]
    fn the_levels_follow_metamask_and_ask_for_their_percentiles_in_level_order() {
        assert_eq!(LEVELS.map(|tuning| tuning.level), FeeLevel::ALL);
        assert_eq!(REWARD_PERCENTILES, [10, 20, 30]);
        assert!(LEVELS.windows(2).all(|pair| match pair {
            [slower, faster] => {
                slower.percentile < faster.percentile
                    && slower.tip_pct <= faster.tip_pct
                    && slower.base_fee_pct <= faster.base_fee_pct
            }
            _ => false,
        }));
    }

    #[test]
    fn each_level_scales_the_median_of_its_percentile_and_the_next_base_fee() {
        let market = FeeMarket::from_history(&history(RISING), &ETHEREUM).expect("a market");
        assert_eq!(market.base_fee_per_gas(), 12 * GWEI);

        let slow = market.level(FeeLevel::Slow);
        assert_eq!(slow.max_priority_fee_per_gas(), 940_000_000, "94 percent of one gwei");
        assert_eq!(slow.max_fee_per_gas(), 13_200_000_000 + 940_000_000, "110 percent of twelve");

        let normal = market.level(FeeLevel::Normal);
        assert_eq!(normal.max_priority_fee_per_gas(), 1_940_000_000, "97 percent of two gwei");
        assert_eq!(normal.max_fee_per_gas(), 14_400_000_000 + 1_940_000_000);

        let fast = market.level(FeeLevel::Fast);
        assert_eq!(fast.max_priority_fee_per_gas(), 3_920_000_000, "98 percent of four gwei");
        assert_eq!(fast.max_fee_per_gas(), 15 * GWEI + 3_920_000_000);

        let listed: Vec<(FeeLevel, FeePair)> = market.levels().collect();
        assert_eq!(
            listed,
            vec![(FeeLevel::Slow, slow), (FeeLevel::Normal, normal), (FeeLevel::Fast, fast)]
        );
    }

    #[test]
    fn a_faster_level_never_pays_less_than_a_slower_one() {
        let lopsided = r#"{
            "baseFeePerGas":["0x1","0x5"],
            "reward":[["0x0","0x0","0x9"],["0x2","0x2","0x2"],["0x1","0x64","0x3e8"]]
        }"#;
        for params in [ETHEREUM, BSC, POLYGON, ARBITRUM, BASE, AVALANCHE] {
            for body in [RISING, lopsided] {
                let market = FeeMarket::from_history(&history(body), &params).expect("a market");
                let pairs: Vec<FeePair> = market.levels().map(|(_, pair)| pair).collect();
                assert!(
                    pairs.windows(2).all(|pair| match pair {
                        [slower, faster] => faster.clears(*slower),
                        _ => false,
                    }),
                    "{:?}: {pairs:?}",
                    params.chain
                );
            }
        }
    }

    #[test]
    fn empty_blocks_and_a_missing_reward_matrix_fall_back_to_the_network_floor() {
        for params in [ETHEREUM, BSC, POLYGON, ARBITRUM, BASE, AVALANCHE] {
            for body in [
                r#"{"baseFeePerGas":["0x3b9aca00","0x3b9aca00"],"reward":[["0x0","0x0","0x0"]]}"#,
                r#"{"baseFeePerGas":["0x3b9aca00","0x3b9aca00"]}"#,
                "{}",
            ] {
                let market = FeeMarket::from_history(&history(body), &params).expect("a market");
                for (level, pair) in market.levels() {
                    assert_eq!(
                        pair.max_priority_fee_per_gas(),
                        params.min_priority_fee_wei,
                        "{level}"
                    );
                    assert!(pair.max_fee_per_gas() >= pair.max_priority_fee_per_gas());
                }
            }
        }
    }

    #[test]
    fn every_level_is_held_under_the_network_ceiling() {
        let body = r#"{
            "baseFeePerGas":["0x12a05f200","0x12a05f200"],
            "reward":[["0x306252700","0x306252700","0x306252700"]]
        }"#;
        let market = FeeMarket::from_history(&history(body), &AVALANCHE).expect("a market");
        for (level, pair) in market.levels() {
            assert_eq!(pair.max_priority_fee_per_gas(), 150, "{level}");
        }
        assert_eq!(market.level(FeeLevel::Slow).max_fee_per_gas(), 5_500_000_000 + 150);

        let quiet =
            r#"{"baseFeePerGas":["0x12a05f200","0x12a05f200"],"reward":[["0x64","0x64","0x64"]]}"#;
        let market = FeeMarket::from_history(&history(quiet), &AVALANCHE).expect("a market");
        assert_eq!(
            market.level(FeeLevel::Fast).max_priority_fee_per_gas(),
            98,
            "a tip under the ceiling is kept"
        );
    }

    /// BSC's base fee is zero, so each level's cap is its tip and the tip is the
    /// legacy gas price.
    #[test]
    fn a_chain_without_a_base_fee_prices_every_level_as_a_gas_price() {
        let body = r#"{
            "baseFeePerGas":["0x0","0x0"],
            "reward":[["0x2faf080","0x5f5e100","0xbebc200"]]
        }"#;
        let market = FeeMarket::from_history(&history(body), &BSC).expect("a market");
        for (level, pair) in market.levels() {
            assert_eq!(pair.max_fee_per_gas(), pair.max_priority_fee_per_gas(), "{level}");
        }
        assert_eq!(
            market.level(FeeLevel::Slow).max_priority_fee_per_gas(),
            50_000_000,
            "94 percent of the floor is held at the floor"
        );
        assert_eq!(market.level(FeeLevel::Normal).max_priority_fee_per_gas(), 97_000_000);
        assert_eq!(market.level(FeeLevel::Fast).max_priority_fee_per_gas(), 196_000_000);
    }

    #[test]
    fn a_median_ignores_arrival_order_and_averages_the_even_case_without_overflow() {
        assert_eq!(median(&[]), 0);
        assert_eq!(median(&[7]), 7);
        assert_eq!(median(&[10, 20, 30, 40]), 25);
        assert_eq!(median(&[40, 10, 30, 20]), 25, "the order it arrives in does not matter");
        assert_eq!(median(&[u128::MAX, u128::MAX]), u128::MAX, "two maxima do not overflow");
    }

    #[test]
    fn a_gas_limit_is_padded_and_capped_at_the_eip_7825_ceiling() {
        let config = EvmConfig::default();
        let gas = GasLimit::estimated(65_000, &config).expect("a limit");
        assert_eq!(gas.limit(), 78_000);
        assert_eq!(gas.expected(), 65_000, "the fee to expect is priced on the estimate");
        assert_eq!(gas.source(), GasSource::Estimated);

        let transfer = GasLimit::estimated(21_000, &config).expect("a limit");
        assert_eq!(transfer.limit(), 21_000, "a transfer that runs no code is never padded");
        assert_eq!(transfer.expected(), 21_000);

        GasLimit::new(MAX_TX_GAS, GasSource::UserOverride).unwrap();
        assert!(matches!(
            GasLimit::new(MAX_TX_GAS.saturating_add(1), GasSource::UserOverride),
            Err(EvmError::GasAboveCap { cap_gas, .. }) if cap_gas == MAX_TX_GAS
        ));
        for estimate in [MAX_TX_GAS - 1, MAX_TX_GAS] {
            let capped = GasLimit::estimated(estimate, &config).expect("a valid estimate");
            assert_eq!(capped.limit(), MAX_TX_GAS, "only the padding is capped");
            assert_eq!(capped.expected(), estimate);
        }
        assert!(matches!(
            GasLimit::estimated(MAX_TX_GAS.saturating_add(1), &config),
            Err(EvmError::GasAboveCap { cap_gas, .. }) if cap_gas == MAX_TX_GAS
        ));
        assert!(matches!(
            GasLimit::estimated(u64::MAX, &config),
            Err(EvmError::GasAboveCap { .. })
        ));
    }

    #[test]
    fn a_selection_from_another_chain_is_refused_rather_than_ignored() {
        assert_eq!(
            Overrides::price_of(&FeeSelection::NORMAL).expect("normal"),
            Overrides::AUTOMATIC
        );
        assert_eq!(
            Overrides::price_of(&FeeSelection::Level { level: FeeLevel::Fast }).expect("a level"),
            Overrides { level: FeeLevel::Fast, ..Overrides::AUTOMATIC }
        );
        assert!(matches!(
            Overrides::price_of(&FeeSelection::Rate { rate: String::from("5") }),
            Err(EvmError::ForeignFeeSelection)
        ));
        assert!(matches!(
            Overrides::price_of(&FeeSelection::Solana { priority_fee_lamports: String::from("5") }),
            Err(EvmError::ForeignFeeSelection)
        ));
    }

    fn manual(
        gas_limit: Option<&str>,
        max_fee_per_gas: Option<&str>,
        max_priority_fee_per_gas: Option<&str>,
    ) -> FeeSelection {
        FeeSelection::Evm {
            gas_limit: gas_limit.map(str::to_owned),
            max_fee_per_gas: max_fee_per_gas.map(str::to_owned),
            max_priority_fee_per_gas: max_priority_fee_per_gas.map(str::to_owned),
        }
    }

    fn request(fee: FeeSelection, data: Option<&str>) -> TxRequest {
        let builder =
            TxRequestBuilder::new(1, "0x0", AssetId::Native(ChainId::Ethereum), Amount::ZERO)
                .fee(fee)
                .nonce(7);
        match data {
            Some(data) => builder.data(data).build(),
            None => builder.build(),
        }
    }

    #[test]
    fn a_custom_fee_and_the_requests_overrides_are_read_into_values_once() {
        let read = Overrides::of(&request(
            manual(Some("0x5208"), Some("0x1"), Some("1")),
            Some("0xdeadbeef"),
        ))
        .expect("a custom fee");
        assert_eq!(
            read,
            Overrides {
                level: FeeLevel::Normal,
                gas_limit: Some(21_000),
                max_fee_per_gas: Some(1),
                max_priority_fee_per_gas: Some(1),
                nonce: Some(7),
                calldata: Some(vec![0xde, 0xad, 0xbe, 0xef]),
            }
        );
    }

    #[test]
    fn a_malformed_custom_value_is_refused_as_it_is_read() {
        for (selection, field) in [
            (manual(None, Some("-1"), None), "max fee per gas"),
            (manual(None, None, Some("1.5")), "max priority fee per gas"),
            (manual(Some("21,000"), None, None), "gas limit"),
        ] {
            assert!(
                matches!(
                    Overrides::of(&request(selection, None)),
                    Err(EvmError::NotAnInteger { field: refused }) if refused == field
                ),
                "{field}"
            );
        }
        assert!(matches!(
            Overrides::of(&request(manual(Some("18446744073709551616"), None, None), None)),
            Err(EvmError::IntegerTooWide { field: "gas limit", bits: 64 })
        ));
        assert!(matches!(
            Overrides::of(&request(FeeSelection::NORMAL, Some("0xabc"))),
            Err(EvmError::NotHex { field: "calldata" })
        ));
    }

    #[test]
    fn a_replacement_price_reads_neither_a_nonce_nor_calldata() {
        let price = Overrides::price_of(&manual(Some("21000"), Some("2"), Some("1")))
            .expect("the price reads");
        assert_eq!(price.nonce, None);
        assert_eq!(price.calldata, None);
        assert_eq!(price.max_fee_per_gas, Some(2));
        assert_eq!(price.gas_limit, Some(21_000));
    }

    #[test]
    fn an_override_replaces_only_the_half_it_names() {
        let default = FeePair::new(30 * GWEI, 2 * GWEI).expect("a pair");
        let cap_only = Overrides { max_fee_per_gas: Some(99 * GWEI), ..Overrides::AUTOMATIC };
        let pair = cap_only.pair(default, None).expect("a pair");
        assert_eq!(pair.max_fee_per_gas(), 99 * GWEI);
        assert_eq!(pair.max_priority_fee_per_gas(), 2 * GWEI);
        assert!(!cap_only.pins_the_fee());

        let both = Overrides {
            max_fee_per_gas: Some(1),
            max_priority_fee_per_gas: Some(1),
            ..Overrides::AUTOMATIC
        };
        assert!(both.pins_the_fee());
        assert_eq!(
            both.pair(FeePair::ZERO, None).expect("a pair").max_fee_per_gas(),
            1,
            "a pinned pair reads nothing from the default"
        );
    }

    #[test]
    fn only_the_automatic_halves_are_raised_to_a_slot_floor() {
        let automatic = FeePair::new(30 * GWEI, 2 * GWEI).expect("a pair");
        let floor = FeePair::new(40 * GWEI, GWEI).expect("a pair");
        let raised = Overrides::AUTOMATIC.pair(automatic, Some(floor)).expect("a pair");
        assert_eq!(raised.max_fee_per_gas(), 40 * GWEI, "the cap is raised to the floor");
        assert_eq!(raised.max_priority_fee_per_gas(), 2 * GWEI, "the tip already clears it");

        let pinned = Overrides {
            max_fee_per_gas: Some(GWEI),
            max_priority_fee_per_gas: Some(1),
            ..Overrides::AUTOMATIC
        };
        let kept = pinned.pair(automatic, Some(floor)).expect("a pair");
        assert_eq!(kept.max_fee_per_gas(), GWEI, "a pinned half is never raised");
        assert!(!kept.clears(floor));
    }

    #[test]
    fn a_raised_pair_clears_the_floor_on_both_halves() {
        let floor = FeePair::new(50 * GWEI, 5 * GWEI).expect("a pair");
        let raised =
            FeePair::new(60 * GWEI, GWEI).expect("a pair").raised_to(floor).expect("raised");
        assert!(raised.clears(floor));
        assert_eq!(raised.max_fee_per_gas(), 60 * GWEI, "a half above the floor is kept");
        assert_eq!(raised.max_priority_fee_per_gas(), 5 * GWEI);
    }
}
