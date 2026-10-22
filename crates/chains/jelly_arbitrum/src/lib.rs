//! Arbitrum One, as the spec, the network figures, and the chain entry the EVM
//! framework is run with.

#![deny(clippy::float_arithmetic)]
mod arbitrum;

pub use crate::arbitrum::{ENTRY, PARAMS, SPEC, build, probe};
