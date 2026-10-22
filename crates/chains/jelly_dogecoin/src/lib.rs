//! Dogecoin mainnet over the shared UTXO framework: its spec, its table, its key and
//! address entry, and its adapter build.

#![deny(clippy::float_arithmetic)]
mod dogecoin;

pub use crate::dogecoin::{ENTRY, PARAMS, SPEC, build, probe};
