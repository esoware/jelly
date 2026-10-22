use jelly_core::{FeeRate, wire_enum};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

/// Fraction digits of an exact `BTC/kvB` rendering.
const BTC_SCALE: u32 = 8;
/// Fraction digits this build reads from a base-unit-per-vbyte number. One
/// thousandth of a base unit per vbyte is a quarter of one per kwu, so three digits
/// is already finer than the stored unit.
const VBYTE_SCALE: u32 = 3;
/// Base units per kilo-virtual-byte in one base unit per 1000 weight units.
const PER_KWU_IN_PER_KVB: u128 = 4;

wire_enum! {
    /// The estimator `blockchain.estimatefee` runs.
    pub enum EstimationMode {
        Conservative => "CONSERVATIVE",
        Economical => "ECONOMICAL",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeReading {
    Rate(FeeRate),
    /// The server has no estimate for this target and said so with a negative
    /// number. This is not a rate of zero and must never be used as one.
    Absent,
}

impl FeeReading {
    pub const fn rate(self) -> Option<FeeRate> {
        match self {
            Self::Rate(rate) => Some(rate),
            Self::Absent => None,
        }
    }
}

/// One `[fee rate, total vsize]` bucket of `mempool.get_fee_histogram`.
///
/// The buckets arrive highest rate first and each vsize is the total virtual
/// size of that band, not a running total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistogramBucket {
    pub fee_rate: FeeRate,
    pub vsize: u64,
}

impl<'de> Deserialize<'de> for HistogramBucket {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (rate, vsize) = <(Box<RawValue>, u64)>::deserialize(deserializer)?;
        let Some(FeeReading::Rate(fee_rate)) = read_sat_per_vb(rate.get()) else {
            return Err(D::Error::custom("a histogram bucket has an unreadable fee rate"));
        };
        Ok(Self { fee_rate, vsize })
    }
}

/// The three relay floors `mempool.get_info` reports, already converted out of
/// `BTC/kvB`. A floor is `None` when the server left it out or answered with a
/// negative number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_field_names,
    reason = "the fields keep the names of Bitcoin Core's mempoolminfee, minrelaytxfee, and incrementalrelayfee"
)]
pub struct MempoolInfo {
    pub mempool_min_fee: Option<FeeRate>,
    pub min_relay_tx_fee: Option<FeeRate>,
    pub incremental_relay_fee: Option<FeeRate>,
}

impl MempoolInfo {
    /// The configured minimum relay fee, or the mempool's own floor when it is
    /// full and charging more. `None` when the server stated neither.
    pub fn relay_floor(self) -> Option<FeeRate> {
        // `None` orders below every rate, so this is the higher of the stated
        // floors.
        self.mempool_min_fee.max(self.min_relay_tx_fee)
    }
}

impl<'de> Deserialize<'de> for MempoolInfo {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Fulcrum copies only the keys its daemon returned, and Litecoin Core's
        // getmempoolinfo has no incrementalrelayfee.
        #[derive(Deserialize)]
        struct Wire {
            mempoolminfee: Option<Box<RawValue>>,
            minrelaytxfee: Option<Box<RawValue>>,
            incrementalrelayfee: Option<Box<RawValue>>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            mempool_min_fee: read_relay_fee::<D>(wire.mempoolminfee.as_deref())?,
            min_relay_tx_fee: read_relay_fee::<D>(wire.minrelaytxfee.as_deref())?,
            incremental_relay_fee: read_relay_fee::<D>(wire.incrementalrelayfee.as_deref())?,
        })
    }
}

/// A relay fee the server declines to state, with a negative number or by
/// leaving it out, reads as `None` rather than an error, so the caller falls back
/// to the chain's compiled-in constant.
fn read_relay_fee<'de, D: Deserializer<'de>>(
    raw: Option<&RawValue>,
) -> Result<Option<FeeRate>, D::Error> {
    let Some(raw) = raw else { return Ok(None) };
    match read_btc_per_kvb(raw.get()) {
        Some(reading) => Ok(reading.rate()),
        None => Err(D::Error::custom("a mempool fee is not a number this build can read")),
    }
}

/// Reads a `BTC/kvB` number token, which is what `blockchain.estimatefee` and
/// `mempool.get_info` answer with.
///
/// One satoshi per kvB is exactly a quarter of one satoshi per 1000 weight
/// units, so the whole conversion is integer.
pub(crate) fn read_btc_per_kvb(token: &str) -> Option<FeeReading> {
    read_rate(token, BTC_SCALE)
}

/// Reads a base-unit-per-virtual-byte number token, the unit of
/// `mempool.get_fee_histogram` and of mempool.space-style REST fee services.
pub fn read_sat_per_vb(token: &str) -> Option<FeeReading> {
    read_rate(token, VBYTE_SCALE)
}

/// Parses decimal or exponent notation into base units per 1000 weight units,
/// rounding up at `scale` and again at the division by four.
///
/// Python-based servers send exponents, which `Amount::parse` rejects, and it
/// does not round up.
fn read_rate(token: &str, scale: u32) -> Option<FeeReading> {
    let text = token.trim();
    let (is_negative, unsigned) = match text.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix('+').unwrap_or(text)),
    };
    let (mantissa, exponent) = split_exponent(unsigned)?;
    let (value, fraction_digits) = read_decimal(mantissa)?;
    if is_negative && value != 0 {
        return Some(FeeReading::Absent);
    }
    let shift = i64::from(exponent)
        .checked_sub(i64::from(fraction_digits))?
        .checked_add(i64::from(scale))?;
    let scaled = shift_ceil(value, shift)?;
    let per_kwu = scaled.div_ceil(PER_KWU_IN_PER_KVB);
    u64::try_from(per_kwu).ok().map(|rate| FeeReading::Rate(FeeRate::from_sat_per_kwu(rate)))
}

/// Splits `1.5e-8` into its mantissa and its base-ten exponent.
fn split_exponent(text: &str) -> Option<(&str, i32)> {
    match text.split_once(['e', 'E']) {
        None => Some((text, 0)),
        Some((mantissa, exponent)) => Some((mantissa, exponent.parse().ok()?)),
    }
}

/// Reads `12.345` as the integer `12345` and its `3` fraction digits.
fn read_decimal(text: &str) -> Option<(u128, u16)> {
    let (integer, fraction) = text.split_once('.').unwrap_or((text, ""));
    if integer.is_empty() {
        return None;
    }
    let value = integer.chars().chain(fraction.chars()).try_fold(0u128, |value, digit| {
        value.checked_mul(10)?.checked_add(u128::from(digit.to_digit(10)?))
    })?;
    Some((value, u16::try_from(fraction.len()).ok()?))
}

/// `value * 10^shift`, rounding up when `shift` is negative.
fn shift_ceil(value: u128, shift: i64) -> Option<u128> {
    if shift >= 0 {
        let power = 10u128.checked_pow(u32::try_from(shift).ok()?)?;
        return value.checked_mul(power);
    }
    let power = 10u128.checked_pow(u32::try_from(shift.unsigned_abs()).ok()?)?;
    Some(value.div_ceil(power))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rate(sat_per_kwu: u64) -> FeeReading {
        FeeReading::Rate(FeeRate::from_sat_per_kwu(sat_per_kwu))
    }

    #[test]
    fn one_unit_per_vbyte_is_two_hundred_and_fifty_per_kwu() {
        assert_eq!(read_btc_per_kvb("0.00001"), Some(rate(250)));
        assert_eq!(read_sat_per_vb("1"), Some(rate(250)));
    }

    #[test]
    fn a_python_exponent_reads_the_same_as_a_plain_decimal() {
        assert_eq!(read_btc_per_kvb("1e-05"), read_btc_per_kvb("0.00001"));
        assert_eq!(read_btc_per_kvb("1E-5"), read_btc_per_kvb("0.00001"));
        assert_eq!(read_btc_per_kvb("1e+1"), read_btc_per_kvb("10"));
    }

    #[test]
    fn a_rate_finer_than_the_stored_unit_rounds_up_never_down() {
        // A fraction of a sat/kwu left over at the division by four must not
        // vanish.
        assert_eq!(read_sat_per_vb("1.002"), Some(rate(251)));
        assert_eq!(read_sat_per_vb("1.001"), Some(rate(251)));
        assert_eq!(read_btc_per_kvb("0.000000001"), Some(rate(1)));
    }

    #[test]
    fn a_negative_estimate_is_absent_not_zero() {
        assert_eq!(read_btc_per_kvb("-1"), Some(FeeReading::Absent));
        assert_eq!(read_btc_per_kvb("-1").and_then(FeeReading::rate), None);
    }

    #[test]
    fn negative_zero_is_a_zero_rate_not_an_absent_estimate() {
        for token in ["-0", "-0.0", "-0.00000000", "-0e-8", "-0.0E+8"] {
            assert_eq!(read_btc_per_kvb(token), Some(rate(0)), "{token}");
            assert_eq!(read_sat_per_vb(token), Some(rate(0)), "{token}");
        }
        assert_eq!(read_btc_per_kvb("-0.000000001"), Some(FeeReading::Absent));
    }

    #[test]
    fn a_token_that_is_not_a_number_is_refused() {
        for token in ["null", "", "0x10", ".5", "1..2", "1e", "1e1e1", "12a"] {
            assert_eq!(read_btc_per_kvb(token), None, "{token}");
        }
    }

    #[test]
    fn the_relay_floor_is_the_higher_of_the_two_minimums() {
        let quiet = MempoolInfo {
            mempool_min_fee: Some(FeeRate::from_sat_per_kwu(250)),
            min_relay_tx_fee: Some(FeeRate::from_sat_per_kwu(250)),
            incremental_relay_fee: Some(FeeRate::from_sat_per_kwu(250)),
        };
        assert_eq!(quiet.relay_floor(), Some(FeeRate::from_sat_per_kwu(250)));
        let busy = MempoolInfo { mempool_min_fee: Some(FeeRate::from_sat_per_kwu(868)), ..quiet };
        assert_eq!(busy.relay_floor(), Some(FeeRate::from_sat_per_kwu(868)));
    }

    #[test]
    fn the_relay_floor_is_the_one_stated_or_none() {
        let only_relay = MempoolInfo {
            mempool_min_fee: None,
            min_relay_tx_fee: Some(FeeRate::from_sat_per_kwu(250)),
            incremental_relay_fee: None,
        };
        assert_eq!(only_relay.relay_floor(), Some(FeeRate::from_sat_per_kwu(250)));
        let silent = MempoolInfo { min_relay_tx_fee: None, ..only_relay };
        assert_eq!(silent.relay_floor(), None);
    }
}
