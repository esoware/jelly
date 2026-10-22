//! TRON mainnet: the chain's entry, its address codec and key formats, and its adapter.
//!
//! Transactions are built locally and their `raw_data` bytes are kept unchanged
//! through preview, signing and broadcast, because re-encoding the protobuf can
//! change the txid and invalidate the signature.
//!
//! A send is persisted before broadcast, and after an ambiguous failure the
//! identical bytes are retried. A rebuild is safe only once head-block time has
//! reached the expiration and no node holds the transaction.

#![deny(clippy::float_arithmetic)]
mod abi;
mod adapter;
mod address;
mod broadcast;
mod client;
mod config;
mod cursor;
mod entry;
mod error;
mod fee;
#[cfg(test)]
mod fixtures;
mod gate;
mod history;
mod identity;
mod node;
mod params;
mod payload;
mod pending;
mod preview;
mod proto;
mod raw;
mod route;
mod send;
mod shape;
mod sign;
mod state;
mod sync;
#[cfg(feature = "test-util")]
pub mod test_util;
mod token;
mod walk;
mod watched;
mod wiring;

pub use crate::adapter::TronAdapter;
pub use crate::config::TronConfig;
pub use crate::entry::{ENTRY, SPEC};
pub use crate::error::TronError;
pub use crate::identity::TronIdentity;
pub use crate::node::{Answered, PooledNode, TronNode};
pub use crate::route::{Request, Route};
pub use crate::wiring::{build, probe};
