//! The UTXO framework the Bitcoin, Litecoin, Bitcoin Cash, and Dogecoin crates share:
//! the adapter over `jelly_electrum`, with sync, coin selection, building, signing, and
//! fee bumping by replacement or by a child that pays for its parent, plus Bitcoin's
//! address forms and wallet import format, and the one chain entry. Chain differences
//! enter through one `UtxoParams` table and one `UtxoEntry` per chain crate.
//!
//! Money is checked integer base units, and coin selection scores candidates in
//! integers too. Every fee comes from `fee::fee_for` and nothing else.

#![deny(clippy::float_arithmetic)]
mod adapter;
mod address;
mod build;
mod claim;
mod coin;
mod cpfp;
mod cursor;
mod entry;
mod error;
mod fee;
mod fee_api;
#[cfg(test)]
mod fixtures;
mod legacy;
mod original;
mod params;
mod payload;
mod preview;
mod prevout;
mod record;
mod replacement;
mod script;
mod select;
mod sign;
mod slp;
mod sync;
#[cfg(feature = "test-util")]
pub mod test_util;
mod token;
mod unspent;
mod weight;
mod wif;
mod wiring;

pub use crate::adapter::UtxoAdapter;
pub use crate::address::UtxoCodec;
pub use crate::entry::UtxoEntry;
pub use crate::error::UtxoError;
pub use crate::fee_api::{FeeApi, MempoolFeeApi, RecommendedFees};
pub use crate::legacy::{Base58Versions, Legacy, is_legacy};
pub use crate::params::{
    AddressNote, AddressRefusal, Auction, BumpMethod, DustRule, FeeMarket, KnownBlock, Replacement,
    UtxoParams,
};
pub use crate::wif::WifFormat;
pub use crate::wiring::{build, probe};
