//! TON mainnet.

#![deny(clippy::float_arithmetic)]

mod adapter;
mod address;
mod client;
mod config;
mod cursor;
mod entry;
mod error;
#[cfg(test)]
mod fixtures;
mod history;
mod identity;
mod jetton;
mod message;
mod method;
mod node;
mod payload;
mod pending;
mod preview;
mod record;
mod seed;
mod send;
mod sign;
mod state;
mod sync;
mod wallet;
mod watched;
mod wiring;

pub use crate::adapter::TonAdapter;
pub use crate::entry::{ENTRY, SPEC};
pub use crate::error::TonError;
pub use crate::method::Method;
pub use crate::node::TonNode;
pub use crate::wiring::{build, probe};
