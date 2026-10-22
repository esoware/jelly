//! Monero mainnet support: the `CryptoNote` key hierarchy, its addresses and seed words,
//! and the adapter. The view key finds outputs locally. The spend key also detects spends
//! and signs. Account keys exist only during an unlocked session.
//!
//! Remote nodes can omit blocks, misreport tips, or influence decoys. Sync rejects gaps,
//! detects reorgs from block IDs, and scans raw blocks locally. Fee ceilings limit node
//! quotes. Each ring uses one node, which alone receives the transaction and every
//! question about it until it cannot be reached. A rebuild offers an output with the ring
//! it was first offered with.
//!
//! Broadcast persists input reservations first. Only on-chain key images prove spending.
//! Scanning deduplicates outputs by transaction and index, and rejects reused output keys.

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
