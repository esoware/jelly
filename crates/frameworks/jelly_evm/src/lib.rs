//! The EVM framework every EIP-155 network shares: the adapter, the address codec,
//! the chain entry with its key import and export, and the build and probe wiring,
//! each generic over the network's [`EvmParams`].
//!
//! Signed payloads carry their EIP-155 chain id to prevent cross-chain replay.
//! Token metadata comes from the compiled-in registry, never the chain. Amounts
//! are integer base units, narrowed with checks from the node's 256-bit values.

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
