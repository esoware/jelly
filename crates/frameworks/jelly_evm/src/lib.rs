//! The framework every EVM chain crate runs on. Each one plugs in with its
//! [`EvmParams`] and [`EvmEntry`].
//!
//! Signed transactions carry their EIP-155 chain id, so they can't be replayed on
//! another network. Token symbols and decimals come from the compiled-in registry,
//! never the chain, and the node's 256-bit values are narrowed with checks into
//! integer base units.

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
mod etherscan;
mod fee;
#[cfg(test)]
mod fixtures;
mod history;
mod identity;
mod ledger;
mod method;
mod node;
mod params;
mod payload;
mod pending;
mod preview;
mod record;
mod rpc;
mod send;
mod sign;
mod sync;
#[cfg(feature = "test-util")]
pub mod test_util;
mod token;
mod units;
mod watched;
mod wiring;

pub use crate::adapter::{EvmAdapter, EvmAdapterBuilder};
pub use crate::address::EvmCodec;
pub use crate::config::EvmConfig;
pub use crate::entry::EvmEntry;
pub use crate::error::{EvmError, HistoryRefusal};
pub use crate::etherscan::EtherscanHistory;
pub use crate::history::{HistoryApi, HistoryPage, InternalRow, NativeRow, TokenRow};
pub use crate::method::Method;
pub use crate::node::{Answered, EvmNode, PooledNode, RpcCall};
pub use crate::params::EvmParams;
pub use crate::wiring::{build, probe};
