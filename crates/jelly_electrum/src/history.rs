use std::fmt;
use std::fmt::Write as _;

use bitcoin::{Amount, Txid};
use serde::{Deserialize, Deserializer};
use sha2::{Digest as _, Sha256};

/// Electrum history position.
///
/// Positive heights are confirmed blocks. Zero is a mempool entry with confirmed
/// parents. Negative heights are mempool entries with unconfirmed parents, including
/// nonstandard values below -1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confirmation {
    Confirmed { height: u32 },
    Mempool { has_unconfirmed_parents: bool },
}

impl Confirmation {
    pub(crate) fn from_wire(height: i32) -> Self {
        match u32::try_from(height) {
            Ok(0) | Err(_) => Self::Mempool { has_unconfirmed_parents: height < 0 },
            Ok(height) => Self::Confirmed { height },
        }
    }

    /// The protocol's signed height, canonicalised to 0 or -1 for the two mempool
    /// cases so servers that spell unconfirmed differently still agree on the
    /// history digest.
    fn wire_height(self) -> i32 {
        match self {
            Self::Confirmed { height } => i32::try_from(height).unwrap_or(i32::MAX),
            Self::Mempool { has_unconfirmed_parents: false } => 0,
            Self::Mempool { has_unconfirmed_parents: true } => -1,
        }
    }

    pub const fn height(self) -> Option<u32> {
        match self {
            Self::Confirmed { height } => Some(height),
            Self::Mempool { .. } => None,
        }
    }

    pub const fn is_confirmed(self) -> bool {
        matches!(self, Self::Confirmed { .. })
    }
}

/// One entry of `blockchain.scripthash.get_history`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryEntry {
    pub txid: Txid,
    pub confirmation: Confirmation,
    /// The mempool fee, which a server reports for unconfirmed entries only.
    pub fee: Option<Amount>,
}

impl<'de> Deserialize<'de> for HistoryEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            tx_hash: Txid,
            height: i32,
            fee: Option<u64>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            txid: wire.tx_hash,
            confirmation: Confirmation::from_wire(wire.height),
            fee: wire.fee.map(Amount::from_sat),
        })
    }
}

/// A canonical history digest that detects an unchanged address across endpoint
/// changes.
///
/// Electrum's status hash depends on intra-block and mempool ordering, which
/// servers disagree on, so this sorts first. It only compares against earlier
/// Jelly digests.
///
/// This is a disk format. A sync cursor stores the bytes verbatim and compares
/// them on the next pass, so any change to [`HistoryDigest::of`], its sort key or
/// its preimage, invalidates every stored cursor and costs each address one full
/// pass.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HistoryDigest([u8; Self::LEN]);

impl HistoryDigest {
    pub const LEN: usize = 32;

    /// `None` when the address has no history.
    ///
    /// Entries sort confirmed first by height, then mempool entries with confirmed
    /// parents, then those with an unconfirmed parent, with txid breaking ties.
    /// The preimage is the `txid:height:` run the protocol's own status uses.
    pub fn of(history: &[HistoryEntry]) -> Option<Self> {
        if history.is_empty() {
            return None;
        }
        let mut sorted_entries: Vec<&HistoryEntry> = history.iter().collect();
        sorted_entries.sort_unstable_by_key(|entry| {
            let confirmation = entry.confirmation;
            // The absolute value keeps both orders, since confirmed heights ascend
            // and the mempool's 0 and -1 become 0 and 1.
            (
                u8::from(!confirmation.is_confirmed()),
                confirmation.wire_height().unsigned_abs(),
                entry.txid,
            )
        });
        let mut preimage = String::new();
        for entry in sorted_entries {
            let _ = write!(preimage, "{}:{}:", entry.txid, entry.confirmation.wire_height());
        }
        Some(Self(Sha256::digest(preimage.as_bytes()).into()))
    }

    /// A cursor stores these bytes verbatim.
    pub const fn from_bytes(bytes: [u8; Self::LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; Self::LEN] {
        &self.0
    }
}

impl fmt::Debug for HistoryDigest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HistoryDigest({})", hex::encode(self.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_protocol_heights_read_into_two_cases() {
        assert_eq!(Confirmation::from_wire(2473), Confirmation::Confirmed { height: 2473 });
        assert_eq!(
            Confirmation::from_wire(0),
            Confirmation::Mempool { has_unconfirmed_parents: false }
        );
        assert_eq!(
            Confirmation::from_wire(-1),
            Confirmation::Mempool { has_unconfirmed_parents: true }
        );
        // No conforming server sends this, and it means the same thing.
        assert_eq!(Confirmation::from_wire(-7), Confirmation::from_wire(-1));
    }

    #[test]
    fn the_wire_height_is_canonical_in_both_directions() {
        for height in [-7i32, -1, 0, 1, 2473, i32::MAX] {
            let confirmation = Confirmation::from_wire(height);
            assert_eq!(Confirmation::from_wire(confirmation.wire_height()), confirmation);
        }
        assert_eq!(Confirmation::from_wire(-7).wire_height(), -1);
        assert_eq!(Confirmation::from_wire(0).height(), None);
        assert_eq!(Confirmation::from_wire(9).height(), Some(9));
    }

    #[test]
    fn a_digest_round_trips_through_the_bytes_a_cursor_stores() {
        let digest = HistoryDigest::from_bytes([7u8; 32]);
        assert_eq!(HistoryDigest::from_bytes(*digest.as_bytes()), digest);
        assert_eq!(format!("{digest:?}"), format!("HistoryDigest({})", "07".repeat(32)));
    }

    #[test]
    fn an_empty_history_has_no_digest() {
        assert_eq!(HistoryDigest::of(&[]), None);
    }
}
