//! Litecoin mainnet.

#![deny(clippy::float_arithmetic)]
mod litecoin;

pub use crate::litecoin::{ENTRY, PARAMS, SPEC, build, probe};
