use std::fmt;
use std::iter::repeat_n;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

/// An asset quantity in base units. The asset supplies its decimals through
/// `AssetId::decimals`.
///
/// Arithmetic methods state their overflow behavior. There are no `Add` or `Sub`
/// implementations.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Amount(#[cfg_attr(feature = "ipc", specta(type = String))] u128);

impl Amount {
    pub const ZERO: Self = Self(0);
    pub const MAX: Self = Self(u128::MAX);

    pub const fn from_base_units(units: u128) -> Self {
        Self(units)
    }

    pub const fn base_units(self) -> u128 {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn checked_add(self, other: Self) -> Option<Self> {
        match self.0.checked_add(other.0) {
            Some(sum) => Some(Self(sum)),
            None => None,
        }
    }

    #[must_use]
    pub const fn checked_sub(self, other: Self) -> Option<Self> {
        match self.0.checked_sub(other.0) {
            Some(difference) => Some(Self(difference)),
            None => None,
        }
    }

    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }

    #[must_use]
    pub fn checked_sum<I: IntoIterator<Item = Self>>(amounts: I) -> Option<Self> {
        amounts.into_iter().try_fold(Self::ZERO, Self::checked_add)
    }

    /// Decimal rendering with trailing fraction zeros removed. This and
    /// [`Amount::parse`] are the workspace's only decimal conversions for amounts.
    pub fn format(self, decimals: u8) -> String {
        decimal_text(self.0, usize::from(decimals))
    }

    /// Parses a decimal string at `decimals` precision. The caller trims whitespace,
    /// so a leading or trailing space is rejected.
    pub fn parse(text: &str, decimals: u8) -> Result<Self, DecimalError> {
        scaled_units(text, usize::from(decimals)).map(Self)
    }
}

impl From<u64> for Amount {
    fn from(units: u64) -> Self {
        Self(u128::from(units))
    }
}

/// Base units as a decimal string, the same as the wire format. User-facing text
/// uses [`Amount::format`].
impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl Serialize for Amount {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse::<u128>().map(Self).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum DecimalError {
    #[error("Enter a value.")]
    Empty,
    #[error("Enter a digit at position {}.", position.saturating_add(1))]
    InvalidDigit { position: usize },
    #[error("Use only one decimal point.")]
    MultipleDecimalPoints,
    #[error("Enter digits on both sides of the decimal point.")]
    MissingDigits,
    #[error("This has {got} decimal places. Use at most {max}.")]
    TooPrecise { max: usize, got: usize },
    #[error("Enter a smaller value.")]
    Overflow,
}

/// Fee rate in base units per 1000 weight units. One base unit per virtual byte equals
/// 250 per kwu, so three decimal places represent every rate exactly without floats.
///
/// "sat" in the method names stands for the chain's base unit, whatever the chain calls it.
///
/// It has no `Display`, because the unit's name belongs to the chain. Callers pair
/// [`FeeRate::format_sat_per_vb`] with `ChainSpec::fee_rate_unit`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FeeRate(u64);

impl FeeRate {
    pub const ZERO: Self = Self(0);

    /// Fraction digits of an exact per-virtual-byte rendering. The step is 0.004.
    const VBYTE_SCALE: usize = 3;

    /// Thousandths of a base unit per virtual byte in one per kwu, which is 1000 / 250.
    const MILLI_VB_PER_KWU: u128 = 4;

    pub const fn from_sat_per_kwu(rate: u64) -> Self {
        Self(rate)
    }

    pub const fn sat_per_kwu(self) -> u64 {
        self.0
    }

    pub fn format_sat_per_vb(self) -> String {
        let milli_vb = u128::from(self.0).saturating_mul(Self::MILLI_VB_PER_KWU);
        decimal_text(milli_vb, Self::VBYTE_SCALE)
    }

    /// Parses a per-virtual-byte decimal with at most three fraction digits. A value
    /// that is not a multiple of 0.004 rounds up to the next whole unit per kwu.
    pub fn parse_sat_per_vb(text: &str) -> Result<Self, DecimalError> {
        let milli_vb = scaled_units(text, Self::VBYTE_SCALE)?;
        let kwu = milli_vb.div_ceil(Self::MILLI_VB_PER_KWU);
        u64::try_from(kwu).map(Self).map_err(|_| DecimalError::Overflow)
    }
}

/// Renders `units` as a decimal with `scale` fraction places, trailing fraction zeros
/// removed.
fn decimal_text(units: u128, scale: usize) -> String {
    let width = scale.saturating_add(1);
    let digits = format!("{units:0>width$}");
    let (integer_part, fraction_part) = digits.split_at(digits.len().saturating_sub(scale));
    let fraction_part = fraction_part.trim_end_matches('0');
    if fraction_part.is_empty() {
        integer_part.to_owned()
    } else {
        format!("{integer_part}.{fraction_part}")
    }
}

/// Validates a decimal string and returns its value scaled to `scale` fraction places.
fn scaled_units(text: &str, scale: usize) -> Result<u128, DecimalError> {
    if text.is_empty() {
        return Err(DecimalError::Empty);
    }
    // Every character before the first invalid one is ASCII, so its byte offset is also
    // its character position.
    if let Some((position, _)) = text.char_indices().find(|&(_, c)| !c.is_ascii_digit() && c != '.')
    {
        return Err(DecimalError::InvalidDigit { position });
    }
    let (integer_part, fraction_part) = match text.split_once('.') {
        None => (text, ""),
        Some((integer_part, fraction_part)) => {
            if fraction_part.contains('.') {
                return Err(DecimalError::MultipleDecimalPoints);
            }
            if integer_part.is_empty() || fraction_part.is_empty() {
                return Err(DecimalError::MissingDigits);
            }
            (integer_part, fraction_part)
        }
    };
    if fraction_part.len() > scale {
        return Err(DecimalError::TooPrecise { max: scale, got: fraction_part.len() });
    }
    let mut digits = String::with_capacity(integer_part.len().saturating_add(scale));
    digits.push_str(integer_part);
    digits.push_str(fraction_part);
    digits.extend(repeat_n('0', scale.saturating_sub(fraction_part.len())));
    digits.parse::<u128>().map_err(|_| DecimalError::Overflow)
}
