//! BNB Smart Chain.

#![deny(clippy::float_arithmetic)]
mod bsc;

pub use crate::bsc::{ENTRY, PARAMS, SPEC, build, probe};
