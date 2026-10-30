//! The UTXO framework behind the Bitcoin, Litecoin, Bitcoin Cash, and Dogecoin crates. It
//! syncs over Electrum, selects coins, builds and signs, and bumps fees by replacement or
//! child-pays-for-parent. Each chain plugs in with its `UtxoParams` and `UtxoEntry`.
//!
//! Money stays in checked integer base units, coin selection included, and every fee
//! comes from `fee::fee_for`.

#![deny(clippy::float_arithmetic)]
mod adapter;
mod address;
mod build;
mod claim;
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
mod utxo;
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
