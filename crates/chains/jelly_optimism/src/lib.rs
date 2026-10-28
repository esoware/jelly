//! OP Mainnet.

#![deny(clippy::float_arithmetic)]
mod optimism;

pub use crate::optimism::{ENTRY, PARAMS, SPEC, build, probe};
