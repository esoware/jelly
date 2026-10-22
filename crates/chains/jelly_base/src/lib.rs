//! Base, as the spec, the network figures, and the chain entry the EVM framework
//! is run with.

#![deny(clippy::float_arithmetic)]
mod base;

pub use crate::base::{ENTRY, PARAMS, SPEC, build, probe};
