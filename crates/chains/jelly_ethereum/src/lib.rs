//! Ethereum mainnet, as the spec, the network figures, and the chain entry the EVM
//! framework is run with.

#![deny(clippy::float_arithmetic)]
mod ethereum;

pub use crate::ethereum::{ENTRY, PARAMS, SPEC, build, probe};
