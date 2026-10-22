//! The `CashAddr` format, a base32 payload of a version byte and a hash under a
//! 40-bit BCH-code checksum that also covers the network prefix.
//!
//! The prefix is part of the checksum but optional in text, so text without one is
//! read against the prefix the caller expects.

use std::iter;

use bitcoin::bech32::{ByteIterExt as _, Fe32, Fe32IterExt as _};

const SEPARATOR: char = ':';

const CHECKSUM_GROUPS: usize = 8;

/// The version byte's top bit is reserved and must be clear.
const RESERVED_BIT: u8 = 0x80;

/// Hash lengths in bytes, indexed by the version byte's low three bits.
const HASH_LENGTHS: [usize; 8] = [20, 24, 28, 32, 40, 48, 56, 64];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CashAddr {
    /// Lowercase.
    pub(crate) prefix: String,
    /// The type in bits 3 to 6 and the hash size in bits 0 to 2.
    version: u8,
    pub(crate) hash: Vec<u8>,
}

/// Why text is not a `CashAddr`. Positions are byte offsets into the text as given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    InvalidCharacter { position: usize },
    MixedCase { position: usize },
    Checksum,
    Length,
    ReservedBit,
}

impl CashAddr {
    /// `prefix` is lowercase and `address_type` is the four type bits. `None` for a hash
    /// length the format has no size for.
    pub(crate) fn new(prefix: &str, address_type: u8, hash: &[u8]) -> Option<Self> {
        let size = HASH_LENGTHS.iter().position(|&length| length == hash.len())?;
        let version = (address_type << 3) | u8::try_from(size).ok()?;
        Some(Self { prefix: prefix.to_owned(), version, hash: hash.to_vec() })
    }

    /// The type bits, such as 0 for P2PKH and 1 for P2SH.
    pub(crate) const fn address_type(&self) -> u8 {
        (self.version >> 3) & 0x0f
    }

    /// Lowercase, with the prefix.
    pub(crate) fn encode(&self) -> String {
        let data: Vec<Fe32> =
            iter::once(self.version).chain(self.hash.iter().copied()).bytes_to_fes().collect();
        let checksum = polymod(
            prefix_values(&self.prefix)
                .chain(data.iter().map(|value| value.to_u8()))
                .chain([0u8; CHECKSUM_GROUPS]),
        );
        let mut text = String::with_capacity(
            self.prefix
                .len()
                .saturating_add(1)
                .saturating_add(data.len())
                .saturating_add(CHECKSUM_GROUPS),
        );
        text.push_str(&self.prefix);
        text.push(SEPARATOR);
        text.extend(data.iter().map(|value| value.to_char()));
        for group in (0..CHECKSUM_GROUPS).rev() {
            let shift = group.saturating_mul(5);
            text.push(Fe32::try_from((checksum >> shift) & 0x1f).map_or('q', Fe32::to_char));
        }
        text
    }

    /// Reads `text`, with or without its prefix. Without one, `default_prefix` is the
    /// prefix the checksum is verified against.
    pub(crate) fn decode(text: &str, default_prefix: &str) -> Result<Self, Error> {
        check_case(text)?;
        let (prefix, payload, offset) = match text.split_once(SEPARATOR) {
            Some((prefix, payload)) => {
                (prefix.to_ascii_lowercase(), payload, prefix.len().saturating_add(1))
            }
            None => (default_prefix.to_owned(), text, 0),
        };
        let values = payload
            .char_indices()
            .map(|(index, character)| {
                Fe32::from_char(character)
                    .map_err(|_| Error::InvalidCharacter { position: offset.saturating_add(index) })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let data_groups = values.len().checked_sub(CHECKSUM_GROUPS).ok_or(Error::Length)?;
        if polymod(prefix_values(&prefix).chain(values.iter().map(|value| value.to_u8()))) != 0 {
            return Err(Error::Checksum);
        }
        let bytes = from_five_bits(values.get(..data_groups).ok_or(Error::Length)?)?;
        let (&version, hash) = bytes.split_first().ok_or(Error::Length)?;
        if version & RESERVED_BIT != 0 {
            return Err(Error::ReservedBit);
        }
        let expected = HASH_LENGTHS.get(usize::from(version & 0x07)).copied();
        if expected != Some(hash.len()) {
            return Err(Error::Length);
        }
        Ok(Self { prefix, version, hash: hash.to_vec() })
    }
}

/// Upper case is accepted so a QR code can use its alphanumeric mode, but a mix of
/// the two is refused, as the specification requires.
fn check_case(text: &str) -> Result<(), Error> {
    let mut letters = text.bytes().enumerate().filter(|(_, byte)| byte.is_ascii_alphabetic());
    let Some((_, first)) = letters.next() else {
        return Ok(());
    };
    match letters.find(|(_, byte)| byte.is_ascii_uppercase() != first.is_ascii_uppercase()) {
        Some((position, _)) => Err(Error::MixedCase { position }),
        None => Ok(()),
    }
}

/// The low five bits of each prefix character, then a zero for the separator.
fn prefix_values(prefix: &str) -> impl Iterator<Item = u8> + '_ {
    prefix.bytes().map(|byte| byte & 0x1f).chain([0])
}

fn polymod(values: impl Iterator<Item = u8>) -> u64 {
    const GENERATORS: [u64; 5] =
        [0x98_f2bc_8e61, 0x79_b76d_99e2, 0xf3_3e5f_b3c4, 0xae_2eab_e2a8, 0x1e_4f43_e470];
    let mut checksum: u64 = 1;
    for value in values {
        let top = checksum >> 35;
        checksum = ((checksum & 0x07_ffff_ffff) << 5) ^ u64::from(value);
        for (bit, generator) in GENERATORS.iter().enumerate() {
            if (top >> bit) & 1 == 1 {
                checksum ^= generator;
            }
        }
    }
    checksum ^ 1
}

/// Regroups five-bit values into bytes.
///
/// `fes_to_bytes` drops the bits left over unread, so they are checked here. They must be
/// fewer than five and all zero, or the text was not written by an encoder.
fn from_five_bits(values: &[Fe32]) -> Result<Vec<u8>, Error> {
    let bytes: Vec<u8> = values.iter().copied().fes_to_bytes().collect();
    let leftover_bits =
        values.len().saturating_mul(5).saturating_sub(bytes.len().saturating_mul(8));
    let is_padding_zero = values.last().is_none_or(|last| {
        u32::try_from(leftover_bits).is_ok_and(|bits| last.to_u8().trailing_zeros() >= bits)
    });
    if leftover_bits >= 5 || !is_padding_zero {
        return Err(Error::Length);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    const VECTORS_JSON: &str = include_str!("../tests/vectors/cashaddr.json");

    #[derive(Deserialize)]
    struct Vectors {
        sized: Vec<Row>,
        token_aware: Vec<Row>,
    }

    #[derive(Deserialize)]
    struct Row {
        address: String,
        #[serde(rename = "type")]
        address_type: u8,
        hash: String,
    }

    /// Every published vector, whatever its prefix, type, or hash size, decodes to
    /// its payload, encodes back to itself, and is what its payload builds.
    #[test]
    fn the_published_vectors_round_trip() {
        let vectors: Vectors = serde_json::from_str(VECTORS_JSON).unwrap();
        assert_eq!(vectors.sized.len(), 32);
        for row in vectors.sized.iter().chain(&vectors.token_aware) {
            let decoded = CashAddr::decode(&row.address, "unused").unwrap();
            assert_eq!(decoded.address_type(), row.address_type, "{}", row.address);
            assert_eq!(hex::encode(&decoded.hash), row.hash, "{}", row.address);
            assert_eq!(decoded.encode(), row.address);

            let upper = row.address.to_uppercase();
            assert_eq!(CashAddr::decode(&upper, "unused").unwrap(), decoded, "{upper}");

            let (prefix, payload) = row.address.split_once(':').unwrap();
            let hash = hex::decode(&row.hash).unwrap();
            assert_eq!(CashAddr::new(prefix, row.address_type, &hash).as_ref(), Some(&decoded));
            assert_eq!(CashAddr::decode(payload, prefix).unwrap(), decoded, "{payload}");
            assert_eq!(
                CashAddr::decode(payload, "bitcoincash").is_ok(),
                prefix == "bitcoincash",
                "the prefix is part of the checksum"
            );
        }
    }

    #[test]
    fn a_hash_of_no_listed_size_builds_nothing() {
        assert_eq!(CashAddr::new("bitcoincash", 0, &[0; 21]), None);
        assert_eq!(CashAddr::new("bitcoincash", 0, &[]), None);
    }

    #[test]
    fn a_changed_character_breaks_the_checksum() {
        let vectors: Vectors = serde_json::from_str(VECTORS_JSON).unwrap();
        for row in &vectors.sized {
            let mut altered = row.address.clone().into_bytes();
            let last = altered.last_mut().unwrap();
            *last = if *last == b'q' { b'p' } else { b'q' };
            let altered = String::from_utf8(altered).unwrap();
            assert_eq!(CashAddr::decode(&altered, "unused"), Err(Error::Checksum), "{altered}");
        }
    }

    #[test]
    fn a_character_outside_the_alphabet_is_named_by_its_position() {
        assert_eq!(
            CashAddr::decode("bitcoincash:qr6m7j9njldwwzlg9v7v53unlr4jkmx6eylep8ekgb", "unused"),
            Err(Error::InvalidCharacter { position: 53 })
        );
        assert_eq!(
            CashAddr::decode("qr6m7j9njldwwzlg9v7v53unlr4jkmx6eylep8ekgi", "bitcoincash"),
            Err(Error::InvalidCharacter { position: 41 })
        );
    }

    #[test]
    fn a_payload_shorter_than_its_checksum_is_refused() {
        assert_eq!(CashAddr::decode("bitcoincash:qpzry9x", "unused"), Err(Error::Length));
        assert_eq!(CashAddr::decode("", "bitcoincash"), Err(Error::Length));
    }

    /// The specification's checksum vectors, whose payloads are deliberately not
    /// addresses.
    #[test]
    fn the_published_checksum_vectors_verify() {
        for text in [
            "prefix:x64nx6hz",
            "p:gpf8m4h7",
            "bitcoincash:qpzry9x8gf2tvdw0s3jn54khce6mua7lcw20ayyn",
            "bchtest:testnetaddress4d6njnut",
            "bchreg:555555555555555555555555555555555555555555555udxmlmrz",
        ] {
            let (prefix, payload) = text.split_once(':').expect("a prefix");
            let values = payload
                .chars()
                .map(|character| Fe32::from_char(character).expect("base32").to_u8());
            assert_eq!(polymod(prefix_values(prefix).chain(values)), 0, "{text}");
        }
    }

    #[test]
    fn regrouping_round_trips() {
        for length in 0..70usize {
            let bytes: Vec<u8> =
                (0..length).map(|index| u8::try_from(index).unwrap().wrapping_mul(37)).collect();
            let groups: Vec<Fe32> = bytes.iter().copied().bytes_to_fes().collect();
            assert_eq!(from_five_bits(&groups).unwrap(), bytes, "{length}");
        }
    }

    #[test]
    fn nonzero_padding_is_refused() {
        // Twenty-one bytes take thirty-four groups with two bits of padding.
        let mut groups: Vec<Fe32> = [0u8; 21].into_iter().bytes_to_fes().collect();
        assert_eq!(groups.len(), 34);
        if let Some(last) = groups.last_mut() {
            *last = Fe32::P;
        }
        assert_eq!(from_five_bits(&groups), Err(Error::Length));
    }

    #[test]
    fn a_group_left_over_is_refused() {
        let groups: Vec<Fe32> = [0u8; 2].into_iter().bytes_to_fes().take(3).collect();
        assert_eq!(from_five_bits(&groups), Err(Error::Length));
    }

    #[test]
    fn a_mix_of_cases_is_refused_at_the_first_character_that_breaks_it() {
        let text = "bitcoincash:qpm2qsznhks23z7629mms6s4cwef74vcwvy22gdx6A";
        assert_eq!(
            CashAddr::decode(text, "bitcoincash"),
            Err(Error::MixedCase { position: text.len() - 1 })
        );
    }
}
