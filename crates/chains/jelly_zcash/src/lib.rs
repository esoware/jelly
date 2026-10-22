//! Transparent Zcash wallet support using the Zcash consensus libraries.

#![deny(clippy::float_arithmetic)]

mod adapter;
mod address;
mod client;
mod config;
mod cursor;
mod entry;
mod error;
mod fee;
#[cfg(test)]
mod fixtures;
mod identity;
mod node;
mod payload;
mod pending;
mod preview;
mod record;
mod route;
mod send;
mod sign;
mod state;
mod sync;
mod watched;
mod wiring;
mod zat;

pub use crate::adapter::ZcashAdapter;
pub use crate::entry::{ENTRY, SPEC};
pub use crate::error::ZcashError;
pub use crate::node::ZcashNode;
pub use crate::route::Route;
pub use crate::wiring::{build, probe};
