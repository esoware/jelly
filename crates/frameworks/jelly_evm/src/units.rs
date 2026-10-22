use alloy::hex;
use alloy::primitives::U256;

use crate::error::EvmError;

/// A hostile or broken contract can answer `balanceOf` with 2^256-1, so a 256-bit
/// word is narrowed here before it becomes an [`Amount`](jelly_core::Amount).
pub(crate) fn to_u128(what: &'static str, value: U256) -> Result<u128, EvmError> {
    u128::try_from(value).map_err(|_| EvmError::ValueTooWide { what, bits: 128 })
}

pub(crate) fn to_u64(what: &'static str, value: U256) -> Result<u64, EvmError> {
    u64::try_from(value).map_err(|_| EvmError::ValueTooWide { what, bits: 64 })
}

/// Decimal or `0x`-prefixed hex. Anything else, such as a sign or a digit
/// separator, is refused rather than guessed at.
pub(crate) fn parse_integer(field: &'static str, text: &str) -> Result<u128, EvmError> {
    let body = text.trim();
    if body.is_empty() {
        return Err(EvmError::NotAnInteger { field });
    }
    match body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        Some(digits)
            if !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit()) =>
        {
            u128::from_str_radix(digits, 16)
                .map_err(|_| EvmError::IntegerTooWide { field, bits: 128 })
        }
        None if body.bytes().all(|byte| byte.is_ascii_digit()) => {
            body.parse::<u128>().map_err(|_| EvmError::IntegerTooWide { field, bits: 128 })
        }
        Some(_) | None => Err(EvmError::NotAnInteger { field }),
    }
}

pub(crate) fn parse_gas(field: &'static str, text: &str) -> Result<u64, EvmError> {
    let wide = parse_integer(field, text)?;
    u64::try_from(wide).map_err(|_| EvmError::IntegerTooWide { field, bits: 64 })
}

/// `0x`-prefixed or bare hex bytes.
pub(crate) fn parse_hex(field: &'static str, text: &str) -> Result<Vec<u8>, EvmError> {
    let body = text.trim();
    let body = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")).unwrap_or(body);
    if body.is_empty() {
        return Ok(Vec::new());
    }
    hex::decode(body).map_err(|_| EvmError::NotHex { field })
}

pub(crate) fn scale_pct(field: &'static str, value: u128, pct: u64) -> Result<u128, EvmError> {
    value
        .checked_mul(u128::from(pct))
        .and_then(|scaled| scaled.checked_div(100))
        .ok_or(EvmError::FeeOverflow { field })
}

/// Rounds up, because a replacement floor rounded down can land one wei short
/// and be refused.
pub(crate) fn scale_pct_up(field: &'static str, value: u128, pct: u64) -> Result<u128, EvmError> {
    value
        .checked_mul(u128::from(pct))
        .map(|scaled| scaled.div_ceil(100))
        .ok_or(EvmError::FeeOverflow { field })
}

pub(crate) fn cost(field: &'static str, per_gas: u128, gas: u64) -> Result<u128, EvmError> {
    per_gas.checked_mul(u128::from(gas)).ok_or(EvmError::FeeOverflow { field })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_integer_is_read_in_decimal_or_hex_and_nothing_else() {
        assert_eq!(parse_integer("f", "0").expect("zero"), 0);
        assert_eq!(parse_integer("f", " 21000 ").expect("padded"), 21_000);
        assert_eq!(parse_integer("f", "0x5208").expect("hex"), 21_000);
        assert_eq!(parse_integer("f", "0X5208").expect("upper prefix"), 21_000);
        for refused in ["", "  ", "-1", "1_000", "0x", "0xzz", "1.5", "1e9", "+7"] {
            assert!(parse_integer("f", refused).is_err(), "{refused}");
        }
        assert!(matches!(
            parse_integer("f", "340282366920938463463374607431768211456"),
            Err(EvmError::IntegerTooWide { bits: 128, .. })
        ));
    }

    #[test]
    fn a_gas_limit_narrows_to_sixty_four_bits_or_is_refused() {
        assert_eq!(parse_gas("g", "21000").expect("gas"), 21_000);
        assert!(matches!(
            parse_gas("g", "18446744073709551616"),
            Err(EvmError::IntegerTooWide { bits: 64, .. })
        ));
    }

    #[test]
    fn hex_bytes_accept_both_prefix_forms_and_the_empty_body() {
        assert_eq!(parse_hex("d", "0x").expect("empty"), Vec::<u8>::new());
        assert_eq!(parse_hex("d", "").expect("empty"), Vec::<u8>::new());
        assert_eq!(parse_hex("d", "0xdeadBEEF").expect("hex"), vec![0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(parse_hex("d", "deadbeef").expect("bare"), vec![0xde, 0xad, 0xbe, 0xef]);
        assert!(parse_hex("d", "0xabc").is_err(), "an odd number of nibbles is not bytes");
    }

    #[test]
    fn percent_scaling_rounds_down_and_the_replacement_form_rounds_up() {
        assert_eq!(scale_pct("f", 199, 110).expect("scaled"), 218);
        assert_eq!(scale_pct_up("f", 199, 110).expect("scaled"), 219);
        assert_eq!(scale_pct("f", 0, 110).expect("scaled"), 0);
        assert_eq!(scale_pct_up("f", 0, 110).expect("scaled"), 0);
        scale_pct("f", u128::MAX, 110).unwrap_err();
        scale_pct_up("f", u128::MAX, 110).unwrap_err();
    }

    #[test]
    fn a_word_wider_than_the_wallets_base_units_is_refused_not_truncated() {
        assert_eq!(to_u128("w", U256::from(7u64)).expect("fits"), 7);
        assert!(matches!(to_u128("w", U256::MAX), Err(EvmError::ValueTooWide { bits: 128, .. })));
        assert_eq!(to_u64("w", U256::from(7u64)).expect("fits"), 7);
        assert!(matches!(
            to_u64("w", U256::from(u128::MAX)),
            Err(EvmError::ValueTooWide { bits: 64, .. })
        ));
    }
}
