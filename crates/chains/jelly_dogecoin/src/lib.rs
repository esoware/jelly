//! Dogecoin mainnet.

#![deny(clippy::float_arithmetic)]
mod dogecoin;

pub use crate::dogecoin::{ENTRY, PARAMS, SPEC, build, probe};
