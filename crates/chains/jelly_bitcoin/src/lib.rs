//! Bitcoin mainnet.

#![deny(clippy::float_arithmetic)]
mod bitcoin;

pub use crate::bitcoin::{ENTRY, PARAMS, SPEC, build, probe};
