//! Bitcoin Cash mainnet.

#![deny(clippy::float_arithmetic)]
mod address;
mod bitcoin_cash;
mod cashaddr;

pub use crate::bitcoin_cash::{ENTRY, PARAMS, SPEC, build, probe};
