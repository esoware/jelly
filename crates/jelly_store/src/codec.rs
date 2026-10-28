//! Every encoding here is a disk format.
//!
//! Closed vocabularies use [`jelly_core`]'s spelling (`as_str`/`parse`, or `key`/`parse_key`
//! for assets), so the wire, log, and column strings are one literal. `tests/it/format.rs`
//! pins the bytes that reach the file.

use jelly_core::Amount;
use rusqlite::Row;

use crate::error::StoreError;

/// Money is a 16-byte big-endian unsigned integer, so blob order is numeric
/// order and a range filter needs no decoding.
const AMOUNT_BYTES: usize = 16;

pub(crate) const fn amount_to_blob(amount: Amount) -> [u8; AMOUNT_BYTES] {
    amount.base_units().to_be_bytes()
}

/// A column and the entity it belongs to, so an error names where it came from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Column {
    entity: &'static str,
    name: &'static str,
}

impl Column {
    pub(crate) const fn new(entity: &'static str, name: &'static str) -> Self {
        Self { entity, name }
    }

    /// For a value this module can't decode itself, such as a `KeySource`, or a failure
    /// that isn't a decode.
    pub(crate) const fn name(self) -> &'static str {
        self.name
    }

    pub(crate) const fn decode_error(self) -> StoreError {
        StoreError::decode(self.entity, self.name)
    }
}

/// `SQLite` integers are signed, so a value above `i64::MAX` is refused rather than
/// truncated.
pub(crate) fn u64_to_db(value: u64, column: Column) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Rejected {
        entity: column.entity,
        constraint: format!("{} <= {}", column.name, i64::MAX),
    })
}

pub(crate) fn optional_u64_to_db(
    value: Option<u64>,
    column: Column,
) -> Result<Option<i64>, StoreError> {
    value.map(|value| u64_to_db(value, column)).transpose()
}

/// A closed vocabulary stored as its wire spelling.
pub(crate) fn wire<T>(
    row: &Row<'_>,
    column: Column,
    parse: fn(&str) -> Option<T>,
) -> Result<T, StoreError> {
    let raw: String = row.get(column.name)?;
    parse(&raw).ok_or_else(|| column.decode_error())
}

pub(crate) fn optional_wire<T>(
    row: &Row<'_>,
    column: Column,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, StoreError> {
    let raw: Option<String> = row.get(column.name)?;
    raw.map(|text| parse(&text).ok_or_else(|| column.decode_error())).transpose()
}

pub(crate) fn amount(row: &Row<'_>, column: Column) -> Result<Amount, StoreError> {
    let raw: Vec<u8> = row.get(column.name)?;
    amount_from_blob(&raw).ok_or_else(|| column.decode_error())
}

pub(crate) fn optional_amount(row: &Row<'_>, column: Column) -> Result<Option<Amount>, StoreError> {
    let raw: Option<Vec<u8>> = row.get(column.name)?;
    raw.map(|bytes| amount_from_blob(&bytes).ok_or_else(|| column.decode_error())).transpose()
}

/// A stored value outside `T`'s range is refused rather than truncated.
pub(crate) fn int<T: TryFrom<i64>>(row: &Row<'_>, column: Column) -> Result<T, StoreError> {
    let raw: i64 = row.get(column.name)?;
    T::try_from(raw).map_err(|_| column.decode_error())
}

pub(crate) fn optional_int<T: TryFrom<i64>>(
    row: &Row<'_>,
    column: Column,
) -> Result<Option<T>, StoreError> {
    let raw: Option<i64> = row.get(column.name)?;
    raw.map(|value| T::try_from(value).map_err(|_| column.decode_error())).transpose()
}

pub(crate) fn sha256(row: &Row<'_>, column: Column) -> Result<[u8; 32], StoreError> {
    let raw: Vec<u8> = row.get(column.name)?;
    <[u8; 32]>::try_from(raw.as_slice()).map_err(|_| column.decode_error())
}

fn amount_from_blob(bytes: &[u8]) -> Option<Amount> {
    let fixed: [u8; AMOUNT_BYTES] = bytes.try_into().ok()?;
    Some(Amount::from_base_units(u128::from_be_bytes(fixed)))
}

#[cfg(test)]
mod tests {
    use jelly_core::Amount;
    use proptest::prelude::{Strategy as _, any, prop_assert, prop_assert_eq, proptest};

    use super::{amount_from_blob, amount_to_blob};

    #[test]
    fn the_amount_blob_is_sixteen_big_endian_bytes() {
        let cases: [(u128, &str); 5] = [
            (0, "00000000000000000000000000000000"),
            (1, "00000000000000000000000000000001"),
            (u128::from(u64::MAX), "0000000000000000ffffffffffffffff"),
            (u128::from(u64::MAX).saturating_add(1), "00000000000000010000000000000000"),
            (u128::MAX, "ffffffffffffffffffffffffffffffff"),
        ];
        for (value, expected) in cases {
            let blob = amount_to_blob(Amount::from_base_units(value));
            assert_eq!(hex::encode(blob), expected);
            assert_eq!(amount_from_blob(&blob), Some(Amount::from_base_units(value)));
        }
        assert_eq!(amount_from_blob(&[0u8; 15]), None);
        assert_eq!(amount_from_blob(&[0u8; 17]), None);
    }

    proptest! {
        #[test]
        fn the_amount_blob_round_trips(value in any::<u128>()) {
            let amount = Amount::from_base_units(value);
            prop_assert_eq!(amount_from_blob(&amount_to_blob(amount)), Some(amount));
        }

        #[test]
        fn blob_order_matches_numeric_order(
            pair in (any::<u128>(), any::<u128>()).prop_map(|(a, b)| (a.min(b), a.max(b)))
        ) {
            let (low, high) = pair;
            prop_assert!(
                amount_to_blob(Amount::from_base_units(low))
                    <= amount_to_blob(Amount::from_base_units(high))
            );
        }
    }
}
