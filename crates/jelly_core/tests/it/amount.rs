use jelly_core::{Amount, DecimalError, FeeRate};
use proptest::{prelude::*, test_runner::RngSeed};
use serde::Deserialize;

const U64_MAX: u128 = 18_446_744_073_709_551_615;
const U64_MAX_PLUS_ONE: u128 = 18_446_744_073_709_551_616;

const PROPERTY_SEED: u64 = 20_261_021;

const VECTORS_JSON: &str = include_str!("../vectors/amount.json");

#[derive(Deserialize)]
struct Vectors {
    format: Vec<FormatCase>,
    parse_ok: Vec<ParseOkCase>,
    parse_err: Vec<ParseErrCase>,
}

#[derive(Deserialize)]
struct FormatCase {
    base_units: Amount,
    decimals: u8,
    text: String,
}

#[derive(Deserialize)]
struct ParseOkCase {
    text: String,
    decimals: u8,
    base_units: Amount,
}

#[derive(Deserialize)]
struct ParseErrCase {
    text: String,
    decimals: u8,
    #[serde(flatten)]
    error: ExpectedError,
}

#[derive(Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
enum ExpectedError {
    Empty,
    InvalidDigit { position: usize },
    MultiplePoints,
    MissingDigits,
    TooPrecise { got: usize },
    Overflow,
}

impl ExpectedError {
    fn at_scale(self, decimals: u8) -> DecimalError {
        match self {
            Self::Empty => DecimalError::Empty,
            Self::InvalidDigit { position } => DecimalError::InvalidDigit { position },
            Self::MultiplePoints => DecimalError::MultipleDecimalPoints,
            Self::MissingDigits => DecimalError::MissingDigits,
            Self::TooPrecise { got } => {
                DecimalError::TooPrecise { max: usize::from(decimals), got }
            }
            Self::Overflow => DecimalError::Overflow,
        }
    }
}

#[expect(clippy::expect_used, reason = "test helper")]
fn vectors() -> Vectors {
    serde_json::from_str(VECTORS_JSON).expect("the vector file parses")
}

mod vectors_file {
    use super::{Amount, FormatCase, ParseErrCase, ParseOkCase, vectors};

    #[test]
    fn format_matches_every_vector() {
        for case in vectors().format {
            let FormatCase { base_units, decimals, text } = case;
            assert_eq!(
                base_units.format(decimals),
                text,
                "format {base_units} at {decimals} decimals"
            );
        }
    }

    #[test]
    fn parse_matches_every_accepted_vector() {
        for case in vectors().parse_ok {
            let ParseOkCase { text, decimals, base_units } = case;
            assert_eq!(
                Amount::parse(&text, decimals),
                Ok(base_units),
                "parse {text} at {decimals} decimals"
            );
        }
    }

    #[test]
    fn parse_rejects_every_rejected_vector_for_the_stated_reason() {
        for case in vectors().parse_err {
            let ParseErrCase { text, decimals, error } = case;
            assert_eq!(
                Amount::parse(&text, decimals),
                Err(error.at_scale(decimals)),
                "parse {text} at {decimals} decimals"
            );
        }
    }
}

mod serde_shape {
    use super::{Amount, U64_MAX, U64_MAX_PLUS_ONE};
    use crate::common::json;

    #[test]
    fn every_amount_is_a_quoted_decimal_string() {
        for units in [0u128, 1, U64_MAX, U64_MAX_PLUS_ONE, u128::MAX] {
            let text = json(&Amount::from_base_units(units));
            assert_eq!(text, format!("\"{units}\""));
            let back: Amount = serde_json::from_str(&text).expect("deserialize");
            assert_eq!(back, Amount::from_base_units(units));
        }
    }

    #[test]
    fn a_bare_number_is_not_an_amount() {
        serde_json::from_str::<Amount>("1").unwrap_err();
        serde_json::from_str::<Amount>("1.5").unwrap_err();
        serde_json::from_str::<Amount>("null").unwrap_err();
    }

    #[test]
    fn a_string_that_is_not_whole_base_units_is_not_an_amount() {
        serde_json::from_str::<Amount>("\"1.5\"").unwrap_err();
        serde_json::from_str::<Amount>("\"-1\"").unwrap_err();
        serde_json::from_str::<Amount>("\"\"").unwrap_err();
    }

    #[test]
    fn display_is_base_units_which_is_the_wire_form() {
        assert_eq!(Amount::from(100_000_000u64).to_string(), "100000000");
        assert_eq!(Amount::from(u64::MAX).base_units(), U64_MAX);
        assert_eq!(Amount::MAX.to_string(), u128::MAX.to_string());
    }
}

mod arithmetic {
    use std::iter;

    use super::Amount;

    #[test]
    fn checked_add_reports_overflow_instead_of_wrapping() {
        assert_eq!(Amount::MAX.checked_add(Amount::from_base_units(1)), None);
        assert_eq!(
            Amount::from_base_units(2).checked_add(Amount::from_base_units(3)),
            Some(Amount::from_base_units(5))
        );
    }

    #[test]
    fn checked_sub_reports_underflow_and_saturating_sub_does_not() {
        let two = Amount::from_base_units(2);
        let three = Amount::from_base_units(3);
        assert_eq!(two.checked_sub(three), None);
        assert_eq!(two.saturating_sub(three), Amount::ZERO);
        assert_eq!(three.checked_sub(two), Some(Amount::from_base_units(1)));
    }

    #[test]
    fn saturating_add_stops_at_the_maximum() {
        assert_eq!(Amount::MAX.saturating_add(Amount::from_base_units(1)), Amount::MAX);
    }

    #[test]
    fn checked_sum_starts_at_zero_and_reports_overflow() {
        let total = Amount::checked_sum([
            Amount::from_base_units(1),
            Amount::from_base_units(2),
            Amount::from_base_units(3),
        ]);
        assert_eq!(total, Some(Amount::from_base_units(6)));
        assert_eq!(Amount::checked_sum(iter::empty()), Some(Amount::ZERO));
        assert_eq!(Amount::checked_sum([Amount::MAX, Amount::from_base_units(1)]), None);
    }

    #[test]
    fn the_default_amount_is_zero_and_one_base_unit_is_not() {
        assert!(Amount::ZERO.is_zero());
        assert!(Amount::default().is_zero());
        assert!(!Amount::from_base_units(1).is_zero());
    }
}

mod non_ascii {
    use super::{Amount, DecimalError};

    #[test]
    fn a_non_ascii_digit_is_rejected_where_it_stands() {
        // U+0660 ARABIC-INDIC DIGIT ZERO, built without a non-ASCII literal.
        let digit = char::from_u32(0x0660).expect("valid scalar");
        let text = format!("1{digit}");
        assert_eq!(Amount::parse(&text, 8), Err(DecimalError::InvalidDigit { position: 1 }));
    }
}

mod fee_rate {
    use super::{DecimalError, FeeRate};

    #[test]
    fn one_sat_per_vb_is_exactly_250_sat_per_kwu() {
        assert_eq!(FeeRate::parse_sat_per_vb("1"), Ok(FeeRate::from_sat_per_kwu(250)));
        assert_eq!(FeeRate::parse_sat_per_vb("0"), Ok(FeeRate::ZERO));
        assert_eq!(FeeRate::from_sat_per_kwu(250).format_sat_per_vb(), "1");
    }

    #[test]
    fn the_smallest_exact_step_is_four_thousandths() {
        let cases = [
            (1u64, "0.004"),
            (2, "0.008"),
            (10, "0.04"),
            (125, "0.5"),
            (250, "1"),
            (251, "1.004"),
            (2500, "10"),
        ];
        for (kwu, text) in cases {
            assert_eq!(FeeRate::from_sat_per_kwu(kwu).format_sat_per_vb(), text);
        }
    }

    #[test]
    fn parse_round_trips_every_exact_step() {
        for kwu in [0u64, 1, 2, 10, 125, 250, 251, 2500, 1_000_000] {
            let rate = FeeRate::from_sat_per_kwu(kwu);
            let text = rate.format_sat_per_vb();
            assert_eq!(FeeRate::parse_sat_per_vb(&text), Ok(rate), "round trip through {text}");
        }
    }

    #[test]
    fn a_rate_between_two_steps_rounds_up_so_it_never_underpays() {
        assert_eq!(FeeRate::parse_sat_per_vb("0.001"), Ok(FeeRate::from_sat_per_kwu(1)));
        assert_eq!(FeeRate::parse_sat_per_vb("0.003"), Ok(FeeRate::from_sat_per_kwu(1)));
        assert_eq!(FeeRate::parse_sat_per_vb("0.005"), Ok(FeeRate::from_sat_per_kwu(2)));
    }

    #[test]
    fn a_fourth_fraction_digit_is_not_exact_and_is_refused() {
        assert_eq!(
            FeeRate::parse_sat_per_vb("1.0004"),
            Err(DecimalError::TooPrecise { max: 3, got: 4 })
        );
    }

    #[test]
    fn the_same_shape_rules_apply_as_to_amount() {
        assert_eq!(FeeRate::parse_sat_per_vb(""), Err(DecimalError::Empty));
        assert_eq!(FeeRate::parse_sat_per_vb("1.2.3"), Err(DecimalError::MultipleDecimalPoints));
        assert_eq!(FeeRate::parse_sat_per_vb("1."), Err(DecimalError::MissingDigits));
        assert_eq!(
            FeeRate::parse_sat_per_vb("1e3"),
            Err(DecimalError::InvalidDigit { position: 1 })
        );
        assert_eq!(FeeRate::parse_sat_per_vb("100000000000000000000"), Err(DecimalError::Overflow));
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        rng_seed: RngSeed::Fixed(PROPERTY_SEED),
        failure_persistence: None,
        ..ProptestConfig::default()
    })]

    #[test]
    fn every_amount_survives_a_decimal_round_trip(units: u128, decimals in 0u8..=38) {
        let amount = Amount::from_base_units(units);
        let text = amount.format(decimals);
        prop_assert_eq!(Amount::parse(&text, decimals), Ok(amount));
    }

    #[test]
    fn a_formatted_amount_is_ascii_digits_and_at_most_one_point(units: u128, decimals in 0u8..=38) {
        let text = Amount::from_base_units(units).format(decimals);
        prop_assert!(text.chars().all(|c| c.is_ascii_digit() || c == '.'));
        prop_assert!(text.matches('.').count() <= 1);
        prop_assert!(!text.is_empty());
        prop_assert!(!text.ends_with('.'));
    }

    #[test]
    fn parsing_arbitrary_text_never_panics(text: String, decimals in 0u8..=38) {
        let _ = Amount::parse(&text, decimals);
        let _ = FeeRate::parse_sat_per_vb(&text);
    }

    #[test]
    fn every_fee_rate_survives_a_sat_per_vb_round_trip(kwu: u64) {
        let rate = FeeRate::from_sat_per_kwu(kwu);
        let text = rate.format_sat_per_vb();
        prop_assert_eq!(FeeRate::parse_sat_per_vb(&text), Ok(rate));
    }
}
