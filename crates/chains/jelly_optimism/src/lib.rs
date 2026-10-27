//! OP Mainnet, as the spec, the network figures, and the chain entry the EVM
//! framework is run with.

#![deny(clippy::float_arithmetic)]
mod optimism;

pub use crate::optimism::{ENTRY, PARAMS, SPEC, build, probe};
