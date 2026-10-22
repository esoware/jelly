//! Avalanche's C-Chain, as the spec, the network figures, and the chain entry the
//! EVM framework is run with.

#![deny(clippy::float_arithmetic)]
mod avalanche;

pub use crate::avalanche::{ENTRY, PARAMS, SPEC, build, probe};
