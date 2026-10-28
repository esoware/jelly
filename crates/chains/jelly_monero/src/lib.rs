//! Monero mainnet.
//!
//! Remote nodes aren't trusted. Sync scans raw blocks locally, refuses a batch with a gap,
//! and locates reorganizations by block id, and a fee quote above a fixed ceiling is
//! refused. A ring, the transaction built on it, and every question about that transaction
//! go to one node, and to another only when that node can't be reached. A rebuild reuses
//! the ring an output was first offered with.
//!
//! A broadcast persists its input reservations first, and only a key image on chain proves
//! a spend.

#![deny(clippy::float_arithmetic)]
mod account;
mod adapter;
mod address;
mod balance;
mod broadcast;
mod byte_count;
mod config;
mod cursor;
mod daemon;
mod decoys;
mod entry;
mod error;
mod fee;
#[cfg(test)]
mod fixtures;
mod identity;
mod keys;
mod node;
mod params;
mod payload;
mod pending;
mod preview;
mod reader;
mod record;
mod send;
mod sign;
mod state;
mod subaddress;
mod sync;
#[cfg(feature = "test-util")]
pub mod test_util;
mod watched;
mod wiring;

pub use crate::account::MoneroAccount;
pub use crate::adapter::{MoneroAdapter, MoneroAdapterBuilder};
pub use crate::balance::MoneroBalance;
pub use crate::broadcast::BroadcastReply;
pub use crate::config::MoneroConfig;
pub use crate::decoys::Distribution;
pub use crate::entry::{ENTRY, SPEC};
pub use crate::error::MoneroError;
pub use crate::fee::FeeLadder;
pub use crate::keys::MoneroKeys;
pub use crate::node::MoneroNode;
pub use crate::wiring::{build, probe};
