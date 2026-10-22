//! Bitcoin Cash mainnet over the shared UTXO framework: its spec, its table, its
//! `CashAddr` codec, its key and address entry, and its adapter build.

#![deny(clippy::float_arithmetic)]
mod address;
mod bitcoin_cash;
mod cashaddr;

pub use crate::bitcoin_cash::{ENTRY, PARAMS, SPEC, build, probe};
