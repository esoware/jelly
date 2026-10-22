//! Polygon, as the spec, the network figures, and the chain entry the EVM
//! framework is run with.

#![deny(clippy::float_arithmetic)]
mod polygon;

pub use crate::polygon::{ENTRY, PARAMS, SPEC, build, probe};
