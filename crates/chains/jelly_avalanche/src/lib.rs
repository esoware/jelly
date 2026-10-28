//! Avalanche C-Chain.

#![deny(clippy::float_arithmetic)]
mod avalanche;

pub use crate::avalanche::{ENTRY, PARAMS, SPEC, build, probe};
