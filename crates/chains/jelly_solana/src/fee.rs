//! What a Solana transfer costs, in integer lamports.
//!
//! The base fee is a protocol constant per signature. The priority fee is a
//! price times the compute-unit limit requested, not the units consumed, which
//! is why the limit is measured rather than guessed generously.

use jelly_core::{Amount, AssetId, FeeLevel};
use parking_lot::Mutex;

use crate::config::NATIVE;
use crate::error::SolanaError;
use crate::route::{Shape, TransferKind};
use crate::rpc::PrioritizationFee;

/// Lamports per signature.
///
/// The fee rate governor is fixed at this figure, so the base fee is computed
/// rather than read. `getFeeForMessage` would cost a round trip and answers
/// `null` once the blockhash ages out.
pub(crate) const LAMPORTS_PER_SIGNATURE: u64 = 5_000;

/// The most compute units one transaction may request.
const MAX_COMPUTE_UNIT_LIMIT: u32 = 1_400_000;

/// Margin added to a simulated compute-unit count, in thousandths.
const COMPUTE_UNIT_MARGIN_PERMILLE: u64 = 200;

/// The scale a compute-unit price is quoted in.
const MICRO_LAMPORTS_PER_LAMPORT: u128 = 1_000_000;

/// The priority fee a price buys over a compute-unit limit, rounded up exactly
/// as the runtime rounds it.
pub(crate) fn priority_fee(micro_lamports_per_unit: u64, compute_unit_limit: u32) -> Option<u64> {
    let total = u128::from(micro_lamports_per_unit).checked_mul(u128::from(compute_unit_limit))?;
    u64::try_from(total.div_ceil(MICRO_LAMPORTS_PER_LAMPORT)).ok()
}

/// The base fee of a single-signer transaction plus its priority fee.
pub(crate) fn total_fee(micro_lamports_per_unit: u64, compute_unit_limit: u32) -> Option<u64> {
    LAMPORTS_PER_SIGNATURE.checked_add(priority_fee(micro_lamports_per_unit, compute_unit_limit)?)
}

/// The highest price whose fee over `compute_unit_limit` stays at or below
/// `ceiling_lamports`. `None` when the limit is zero or the price does not fit
/// in a u64.
fn price_within(ceiling_lamports: u64, compute_unit_limit: u32) -> Option<u64> {
    let room = u128::from(ceiling_lamports)
        .checked_mul(MICRO_LAMPORTS_PER_LAMPORT)?
        .checked_div(u128::from(compute_unit_limit))?;
    u64::try_from(room).ok()
}

/// Nearest-rank percentile over an ascending sample. An empty sample gives zero,
/// which [`Pricing::sampled`] then raises to the level floor.
fn percentile(sorted_ascending: &[u64], percent: u8) -> u64 {
    let count = sorted_ascending.len();
    if count == 0 {
        return 0;
    }
    let count = u64::try_from(count).unwrap_or(u64::MAX);
    let ratio = u64::from(percent.min(100));
    let rank = ratio.saturating_mul(count).div_ceil(100).clamp(1, count);
    let index = usize::try_from(rank.saturating_sub(1)).unwrap_or(0);
    sorted_ascending.get(index).copied().unwrap_or(0)
}

/// The percentile of the recent prioritization fees a level pays. The normal level
/// reads what Trezor Suite reads.
const fn level_percentile(level: FeeLevel) -> u8 {
    match level {
        FeeLevel::Slow => 50,
        FeeLevel::Normal => 75,
        FeeLevel::Fast => 90,
    }
}

/// The lowest price a level pays, in micro-lamports per compute unit.
///
/// Each sample is the cheapest landed price of its slot, so the samples read low,
/// and Trezor Suite floors its price at this figure. A SOL transfer pays about 160
/// lamports for it.
const MIN_LEVEL_MICRO_LAMPORTS_PER_UNIT: u64 = 300_000;

/// The most a manual priority fee may be, 1 SOL, which is Phantom's limit.
pub(crate) const MAX_CHOSEN_PRIORITY_FEE_LAMPORTS: u64 = 1_000_000_000;

/// What a plan pays per compute unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Pricing {
    pub(crate) micro_lamports_per_unit: u64,
    /// True when the sampled price was lowered to the ceiling.
    pub(crate) is_capped: bool,
}

impl Pricing {
    /// The highest price whose priority fee over `compute_unit_limit` stays within
    /// the `lamports` the user chose. The runtime rounds the fee up, so this
    /// charges exactly `lamports` whenever the limit is at most a million units.
    pub(crate) fn chosen(lamports: u64, compute_unit_limit: u32) -> Self {
        Self {
            micro_lamports_per_unit: price_within(lamports, compute_unit_limit).unwrap_or(0),
            is_capped: false,
        }
    }

    /// The level's percentile of the samples, floored, then lowered to whatever
    /// keeps the priority fee within `ceiling_lamports`.
    ///
    /// Each sample is the lowest price paid in one recent slot by a transaction
    /// touching the queried accounts. The ceiling is there because the sample is
    /// the one number on the fee path a remote host chooses.
    pub(crate) fn sampled(
        samples: &[PrioritizationFee],
        level: FeeLevel,
        compute_unit_limit: u32,
        ceiling_lamports: u64,
    ) -> Self {
        let mut values: Vec<u64> = samples.iter().map(|entry| entry.prioritization_fee).collect();
        values.sort_unstable();
        let wanted =
            percentile(&values, level_percentile(level)).max(MIN_LEVEL_MICRO_LAMPORTS_PER_UNIT);
        let highest = price_within(ceiling_lamports, compute_unit_limit).unwrap_or(wanted);
        Self { micro_lamports_per_unit: wanted.min(highest), is_capped: wanted > highest }
    }
}

/// Shown in the preview, because a limit that was not measured is the one
/// number in a fee that can be wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitSource {
    /// Measured by simulating this exact transaction shape.
    Simulated,
    /// Measured earlier in this session for the same shape.
    Cached,
    /// The node could not be asked, so this is the compiled ceiling for the
    /// shape.
    Compiled,
}

impl UnitSource {
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::Simulated => "Measured by simulation",
            Self::Cached => "Measured earlier this session",
            Self::Compiled => "Fallback estimate",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ComputeUnits {
    limit: u32,
    source: UnitSource,
}

impl ComputeUnits {
    /// Refuses a limit of zero, which no transaction can run under.
    pub(crate) const fn new(limit: u32, source: UnitSource) -> Result<Self, SolanaError> {
        if limit == 0 {
            return Err(SolanaError::ComputeUnitsZero);
        }
        Ok(Self { limit, source })
    }

    /// What a simulation consumed, plus the margin, capped.
    ///
    /// The margin covers state that changes before inclusion. Running out of
    /// compute units still charges the fee and moves nothing.
    pub(crate) fn measured(consumed: u64) -> Result<Self, SolanaError> {
        let scale_permille = COMPUTE_UNIT_MARGIN_PERMILLE.saturating_add(1_000);
        let widened = consumed
            .checked_mul(scale_permille)
            .map(|scaled| scaled.div_ceil(1_000))
            .ok_or(SolanaError::Overflow { what: "a compute unit measurement" })?;
        let limit = u32::try_from(widened.min(u64::from(MAX_COMPUTE_UNIT_LIMIT)))
            .unwrap_or(MAX_COMPUTE_UNIT_LIMIT);
        Self::new(limit, UnitSource::Simulated)
    }

    /// The compiled ceiling for `shape`, used when no simulation is available.
    ///
    /// The priority fee is the price times the limit, so a loose limit overpays.
    /// These are ceilings over what each shape consumes on mainnet.
    pub(crate) const fn compiled(shape: Shape) -> Self {
        let limit = match (shape.kind, shape.is_account_creation) {
            (TransferKind::Native, _) => 1_000,
            (TransferKind::Token, false) => 20_000,
            (TransferKind::Token, true) => 45_000,
            (TransferKind::Token2022, false) => 30_000,
            (TransferKind::Token2022, true) => 80_000,
        };
        Self { limit, source: UnitSource::Compiled }
    }

    /// The limit already passed [`ComputeUnits::new`], so nothing is re-checked.
    #[must_use]
    const fn cached(self) -> Self {
        Self { limit: self.limit, source: UnitSource::Cached }
    }

    pub(crate) const fn limit(self) -> u32 {
        self.limit
    }

    pub(crate) const fn source(self) -> UnitSource {
        self.source
    }

    /// True when no measurement chose the limit, so it is the compiled ceiling for
    /// the shape.
    pub(crate) const fn is_fallback(self) -> bool {
        matches!(self.source, UnitSource::Compiled)
    }
}

/// One session's compute-unit measurements, keyed by the shape they were taken
/// against.
///
/// Consumption depends on which instructions run, not on the amount or
/// counterparty, so a measurement serves every later transfer of that shape.
#[derive(Debug, Default)]
pub(crate) struct UnitCache {
    entries: Mutex<Vec<(Shape, ComputeUnits)>>,
}

impl UnitCache {
    pub(crate) fn get(&self, shape: Shape) -> Option<ComputeUnits> {
        self.entries.lock().iter().find(|(seen, _)| *seen == shape).map(|(_, units)| units.cached())
    }

    pub(crate) fn put(&self, shape: Shape, units: ComputeUnits) {
        let mut entries = self.entries.lock();
        if entries.iter().any(|(seen, _)| *seen == shape) {
            return;
        }
        entries.push((shape, units));
    }
}

/// `base_units` of `asset` at the asset's own scale, followed by its symbol.
pub(crate) fn amount_text(asset: AssetId, base_units: u64) -> String {
    format!("{} {}", Amount::from(base_units).format(asset.decimals()), asset.symbol())
}

pub(crate) fn sol_text(lamports: u64) -> String {
    amount_text(NATIVE, lamports)
}

/// Lamports rendered both ways, which is what every fee row in a preview says.
pub(crate) fn lamports_text(lamports: u64) -> String {
    format!("{} ({lamports} lamports)", sol_text(lamports))
}

#[cfg(test)]
mod tests {
    use jelly_core::TokenId;

    use super::*;

    fn samples(values: &[u64]) -> Vec<PrioritizationFee> {
        values.iter().map(|value| PrioritizationFee { prioritization_fee: *value }).collect()
    }

    #[test]
    fn nearest_rank_percentiles_are_exact_in_integers() {
        let values = [1u64, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&values, 0), 1);
        assert_eq!(percentile(&values, 10), 1);
        assert_eq!(percentile(&values, 50), 5);
        assert_eq!(percentile(&values, 75), 8);
        assert_eq!(percentile(&values, 95), 10);
        assert_eq!(percentile(&values, 100), 10);
        assert_eq!(percentile(&values, u8::MAX), 10);
        assert_eq!(percentile(&[], 50), 0);
        assert_eq!(percentile(&[42], 50), 42);
    }

    #[test]
    fn each_level_prices_at_its_percentile_above_the_floor() {
        let busy = samples(&[
            0, 0, 0, 0, 0, 350_000, 5_000_000, 400_000, 1_000_000, 900_000, 600_000, 700_000,
        ]);
        let price = |level| Pricing::sampled(&busy, level, 1_000, u64::MAX).micro_lamports_per_unit;
        assert_eq!(price(FeeLevel::Slow), 350_000, "the median");
        assert_eq!(price(FeeLevel::Normal), 700_000, "the seventy-fifth percentile");
        assert_eq!(price(FeeLevel::Fast), 1_000_000, "the ninetieth percentile");

        for level in FeeLevel::ALL {
            let quiet = Pricing::sampled(&samples(&[0; 20]), level, 1_000, u64::MAX);
            assert_eq!(quiet.micro_lamports_per_unit, MIN_LEVEL_MICRO_LAMPORTS_PER_UNIT, "{level}");
            assert!(!quiet.is_capped);
            assert_eq!(
                Pricing::sampled(&[], level, 1_000, u64::MAX).micro_lamports_per_unit,
                MIN_LEVEL_MICRO_LAMPORTS_PER_UNIT,
                "an empty sample is not an error"
            );
        }
    }

    /// Fifty lamports over a thousand units allows fifty thousand micro-lamports
    /// per unit.
    #[test]
    fn a_price_above_the_ceiling_is_lowered_and_says_so() {
        let one = samples(&[1_000_000]);
        let capped = Pricing::sampled(&one, FeeLevel::Normal, 1_000, 50);
        assert_eq!(capped.micro_lamports_per_unit, 50_000);
        assert!(capped.is_capped);
        assert!(!Pricing::sampled(&one, FeeLevel::Normal, 1_000, 1_000).is_capped);
    }

    #[test]
    fn a_chosen_fee_is_spread_over_the_limit_and_charges_exactly_what_was_chosen() {
        for (lamports, limit) in [(45u64, 45_000u32), (162, 540), (1, 540), (999_999, 1_000_000)] {
            let pricing = Pricing::chosen(lamports, limit);
            assert!(!pricing.is_capped);
            assert_eq!(
                priority_fee(pricing.micro_lamports_per_unit, limit),
                Some(lamports),
                "{lamports}"
            );
        }
        assert_eq!(Pricing::chosen(45, 45_000).micro_lamports_per_unit, 1_000);
        assert_eq!(Pricing::chosen(0, 540).micro_lamports_per_unit, 0);
    }

    #[test]
    fn the_priority_fee_rounds_up_exactly_as_the_runtime_does() {
        assert_eq!(priority_fee(1, 500), Some(1));
        assert_eq!(priority_fee(0, 200_000), Some(0));
        assert_eq!(priority_fee(0, MAX_COMPUTE_UNIT_LIMIT), Some(0));
        assert_eq!(priority_fee(1_000_000, 1), Some(1));
        assert_eq!(priority_fee(10_000, 200_000), Some(2_000));
        assert_eq!(total_fee(10_000, 200_000), Some(7_000));
        assert_eq!(priority_fee(u64::MAX, u32::MAX), None, "overflow is not a wrap");
        assert_eq!(total_fee(u64::MAX, 1_000_000), None, "nor is the base fee on top");
    }

    #[test]
    fn a_measurement_gains_the_margin_and_stops_at_the_per_transaction_cap() {
        let units = ComputeUnits::measured(1_000).expect("a measurement");
        assert_eq!(units.limit(), 1_200);
        assert_eq!(
            ComputeUnits::measured(451).expect("a measurement").limit(),
            542,
            "the margin rounds up"
        );
        assert_eq!(units.source(), UnitSource::Simulated);
        assert!(!units.is_fallback());

        let capped = ComputeUnits::measured(u64::from(u32::MAX)).expect("a capped measurement");
        assert_eq!(capped.limit(), MAX_COMPUTE_UNIT_LIMIT);
    }

    #[test]
    fn zero_compute_units_are_never_a_usable_limit() {
        for source in [UnitSource::Compiled, UnitSource::Simulated] {
            assert!(matches!(ComputeUnits::new(0, source), Err(SolanaError::ComputeUnitsZero)));
            assert_eq!(ComputeUnits::new(1, source).unwrap().limit(), 1);
        }
        assert!(matches!(ComputeUnits::measured(0), Err(SolanaError::ComputeUnitsZero)));
    }

    #[test]
    fn a_cached_measurement_keeps_its_limit_and_says_where_it_came_from() {
        let measured = ComputeUnits::new(1_150, UnitSource::Simulated).expect("a limit");
        let cached = measured.cached();
        assert_eq!(cached.limit(), measured.limit());
        assert_eq!(cached.source(), UnitSource::Cached);
        assert!(!cached.is_fallback(), "a cached measurement is still a measurement");
    }

    #[test]
    fn the_cache_answers_the_second_time_and_keeps_the_first_measurement_per_shape() {
        let cache = UnitCache::default();
        let native = Shape { kind: TransferKind::Native, is_account_creation: false };
        let token = Shape { kind: TransferKind::Token, is_account_creation: false };
        assert_eq!(cache.get(native), None);

        cache.put(native, ComputeUnits::new(500, UnitSource::Simulated).expect("a limit"));
        cache.put(native, ComputeUnits::new(900, UnitSource::Simulated).expect("a limit"));
        let cached = cache.get(native).expect("a measurement");
        assert_eq!(cached.limit(), 500, "the first measurement stands");
        assert_eq!(cached.source(), UnitSource::Cached);
        assert_eq!(cache.get(token), None);
    }

    #[test]
    fn only_the_compiled_ceiling_is_a_fallback() {
        let native = Shape { kind: TransferKind::Native, is_account_creation: false };
        assert!(ComputeUnits::compiled(native).is_fallback());
        let limit = |source| ComputeUnits::new(1_000, source).expect("a limit");
        assert!(limit(UnitSource::Compiled).is_fallback());
        assert!(!limit(UnitSource::Simulated).is_fallback());
        assert!(!limit(UnitSource::Cached).is_fallback());
    }

    #[test]
    fn a_figure_names_its_asset_at_the_assets_own_scale() {
        assert_eq!(sol_text(1_000_000), "0.001 SOL");
        assert_eq!(lamports_text(5_000), "0.000005 SOL (5000 lamports)");
        assert_eq!(amount_text(AssetId::Token(TokenId::UsdcSpl), 1_500_000), "1.5 USDC");
    }
}
