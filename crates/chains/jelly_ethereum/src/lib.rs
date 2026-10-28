//! Ethereum mainnet.

#![deny(clippy::float_arithmetic)]
mod ethereum;

pub use crate::ethereum::{ENTRY, PARAMS, SPEC, build, probe};
