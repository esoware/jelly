//! XRP Ledger mainnet.

#![deny(clippy::float_arithmetic)]

mod adapter;
mod address;
mod client;
mod config;
mod cursor;
mod entry;
mod error;
mod identity;
mod method;
mod node;
mod payload;
mod pending;
mod preview;
mod record;
mod send;
mod sign;
mod state;
mod sync;
mod watched;
mod wiring;

pub use crate::adapter::XrpAdapter;
pub use crate::entry::{ENTRY, SPEC};
pub use crate::error::XrpError;
pub use crate::method::Method;
pub use crate::node::XrpNode;
pub use crate::wiring::{build, probe};
