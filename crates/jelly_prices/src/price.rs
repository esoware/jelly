use std::fmt;

use jelly_core::{Amount, wire_enum};
use serde::{Serialize, Serializer};

/// The price of one unit of something in a fiat currency, in base units of
/// `Price::DECIMALS`. The currency travels beside a price, never inside it.
///
/// A price is money, so it is parsed straight into an integer and crosses IPC as
/// a string of base units, like an `Amount`.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Price(#[cfg_attr(feature = "ipc", specta(type = String))] u128);

/// One whole currency unit in base units.
const SCALE: u128 = 100_000_000;

const _: () = assert!(
    SCALE == 10u128.pow(8) && Price::DECIMALS == 8,
    "the scale is one currency unit at Price::DECIMALS"
);

impl Price {
    /// Fraction digits of a currency unit. A base unit is a hundred millionth of
    /// a currency unit, which prices the cheapest token the wallet holds to seven
    /// significant figures.
    ///
    /// `PRICE_DECIMALS` in `apps/wallet/src/lib/fiat.ts` must stay equal to
    /// this.
    pub const DECIMALS: u8 = 8;

    /// One billion currency units. A quote above this is a broken or hostile
    /// feed rather than a market, and is refused instead of displayed.
    pub(crate) const CEILING: Self = Self(100_000_000_000_000_000);

    pub(crate) const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// The one way a provider's decimal string becomes a price.
    ///
    /// Digits finer than [`Price::DECIMALS`] are dropped, never rounded, so a
    /// displayed price never exceeds the quoted one. Surrounding whitespace is
    /// tolerated.
    pub(crate) fn quote(raw: &str) -> Result<Self, QuoteRejection> {
        let text = truncated(raw).ok_or(QuoteRejection::NotADecimal)?;
        let amount =
            Amount::parse(text, Self::DECIMALS).map_err(|_| QuoteRejection::NotADecimal)?;
        let price = Self(amount.base_units());
        if price.is_zero() {
            return Err(QuoteRejection::Zero);
        }
        if price > Self::CEILING {
            return Err(QuoteRejection::OutOfRange);
        }
        Ok(price)
    }

    /// This price in another currency, given how many units of that currency one
    /// unit of this one buys. Truncates like `Price::quote`, and is `None` on
    /// overflow.
    pub(crate) fn convert(self, rate: Self) -> Option<Self> {
        self.0.checked_mul(rate.0)?.checked_div(SCALE).map(Self)
    }

    /// How many of the other unit one of this one buys, truncated. `None` for zero.
    pub(crate) fn inverse(self) -> Option<Self> {
        SCALE.checked_mul(SCALE)?.checked_div(self.0).map(Self)
    }

    /// Decimal rendering with trailing fraction zeros removed.
    #[cfg(test)]
    pub(crate) fn format(self) -> String {
        Amount::from_base_units(self.0).format(Self::DECIMALS)
    }
}

/// Base units as a decimal string, the wire representation.
impl fmt::Display for Price {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl Serialize for Price {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

/// Keeps at most [`Price::DECIMALS`] fraction digits.
///
/// Only precision is dropped, so a tail past the last kept digit that is not all
/// digits is `None`. Everything else is left for the exact parse to refuse, so a
/// trailing point stays a trailing point.
fn truncated(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let Some((_, fraction)) = trimmed.split_once('.') else {
        return Some(trimmed);
    };
    let Some((kept_bytes, _)) = fraction.char_indices().nth(usize::from(Price::DECIMALS)) else {
        return Some(trimmed);
    };
    let tail = fraction.get(kept_bytes..)?;
    if !tail.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    trimmed.strip_suffix(tail)
}

wire_enum! {
    /// Why one ticker or currency in an answered request has no price or rate.
    ///
    /// A closed vocabulary that carries nothing from the response body, because a
    /// provider's bytes must never reach a log through this crate.
    pub(crate) enum QuoteRejection {
        /// The provider answered without the pair, or without a last trade for it.
        Absent => "absent",
        /// The price is not a decimal number.
        NotADecimal => "not_a_decimal",
        /// The price, or a rate after inversion, is zero, which would show every
        /// balance as worthless.
        Zero => "zero",
        /// The price is above [`Price::CEILING`].
        OutOfRange => "out_of_range",
    }
}

#[cfg(test)]
mod tests {
    use jelly_core::DecimalError;

    use super::*;

    /// Quoted string, and the base units it must become.
    const QUOTES: [(&str, &str); 10] = [
        ("118432.55", "11843255000000"),
        ("4218.07", "421807000000"),
        ("214", "21400000000"),
        ("0.99987", "99987000"),
        ("1", "100000000"),
        ("0.00000001", "1"),
        // Digits finer than the scale are dropped, never rounded up.
        ("0.2456789999", "24567899"),
        ("0.999999999", "99999999"),
        ("  42.5  ", "4250000000"),
        ("1000000000", "100000000000000000"),
    ];

    const REFUSALS: [(&str, QuoteRejection); 12] = [
        ("0", QuoteRejection::Zero),
        ("0.00", QuoteRejection::Zero),
        ("0.000000001", QuoteRejection::Zero),
        ("", QuoteRejection::NotADecimal),
        ("-1.5", QuoteRejection::NotADecimal),
        ("n/a", QuoteRejection::NotADecimal),
        ("1.2.3", QuoteRejection::NotADecimal),
        ("1e9", QuoteRejection::NotADecimal),
        // Truncation drops precision and nothing else, so a decimal point with no
        // digits on one side is still not a number.
        ("1.", QuoteRejection::NotADecimal),
        (".5", QuoteRejection::NotADecimal),
        ("1 000", QuoteRejection::NotADecimal),
        ("1000000000.01", QuoteRejection::OutOfRange),
    ];

    #[test]
    fn every_recorded_quote_parses_to_its_base_units() {
        for (raw, expected) in QUOTES {
            let price = Price::quote(raw).unwrap_or_else(|reason| panic!("{raw}: {reason}"));
            assert_eq!(price.to_string(), expected, "{raw}");
        }
    }

    #[test]
    fn every_recorded_refusal_names_its_reason() {
        for (raw, expected) in REFUSALS {
            let reason = Price::quote(raw).err().unwrap_or_else(|| panic!("{raw} was accepted"));
            assert_eq!(reason, expected, "{raw}");
        }
    }

    #[test]
    fn the_ceiling_is_the_last_price_accepted() {
        assert_eq!(Price::quote("1000000000"), Ok(Price::CEILING));
        assert_eq!(Price::quote("1000000000.00000001"), Err(QuoteRejection::OutOfRange));
        assert_eq!(Price::CEILING.format(), "1000000000");
    }

    #[test]
    fn an_exact_parse_refuses_what_a_quote_truncates() {
        assert_eq!(
            Amount::parse("0.123456789", Price::DECIMALS),
            Err(DecimalError::TooPrecise { max: 8, got: 9 })
        );
        assert_eq!(
            Price::quote("0.123456789").map(|price| price.to_string()),
            Ok("12345678".into())
        );
    }

    #[test]
    fn the_wire_form_is_base_units_as_a_string() {
        let price = Price(11_843_255_000_000);
        let json = serde_json::to_string(&price).expect("a price serializes");
        assert_eq!(json, "\"11843255000000\"");
        assert_eq!(json, format!("\"{price}\""));
    }

    #[test]
    fn a_converted_price_is_the_product_truncated_to_the_scale() {
        let btc = Price(8_485_680_000_000);
        let yen_per_dollar = Price(15_740_700_000);
        assert_eq!(btc.convert(yen_per_dollar).map(Price::format), Some("13357054.3176".into()));

        let tron = Price(33_431_800);
        let euro_per_dollar = Price(88_956_891);
        // 0.334318 times 0.88956891 is 0.29739889885338, and every digit past the
        // eighth is dropped.
        assert_eq!(tron.convert(euro_per_dollar), Some(Price(29_739_889)));

        assert_eq!(Price(1).convert(euro_per_dollar), Some(Price(0)));
        assert_eq!(Price(u128::MAX).convert(yen_per_dollar), None);
    }

    #[test]
    fn an_inverse_is_truncated_and_refuses_zero() {
        assert_eq!(Price::quote("1.12414").map(Price::inverse), Ok(Some(Price(88_956_891))));
        assert_eq!(Price(100_000_000).inverse(), Some(Price(100_000_000)));
        assert_eq!(Price::CEILING.inverse(), Some(Price(0)));
        assert_eq!(Price(0).inverse(), None);
    }

    #[test]
    fn a_quote_refuses_anything_but_digits_past_the_eighth_fraction_digit() {
        for raw in ["1.2345678901e-05", "1.123456789e9", "1.000000001.5", "1.123456789 junk"] {
            assert_eq!(Price::quote(raw), Err(QuoteRejection::NotADecimal), "{raw}");
        }
    }

    #[test]
    fn every_rejection_has_a_distinct_wire_name() {
        for (at, rejection) in QuoteRejection::ALL.into_iter().enumerate() {
            assert!(!rejection.as_str().is_empty());
            for other in QuoteRejection::ALL.into_iter().skip(at.saturating_add(1)) {
                assert_ne!(rejection.as_str(), other.as_str());
            }
        }
    }
}
