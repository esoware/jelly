//! Base.

#![deny(clippy::float_arithmetic)]
mod base;

pub use crate::base::{ENTRY, PARAMS, SPEC, build, probe};
