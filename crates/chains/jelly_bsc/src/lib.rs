//! BNB Smart Chain, as the spec, the network figures, and the chain entry the EVM
//! framework is run with.

#![deny(clippy::float_arithmetic)]
mod bsc;

pub use crate::bsc::{ENTRY, PARAMS, SPEC, build, probe};
