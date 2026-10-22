//! Solana mainnet-beta: the chain's entry, its address codec and key formats, and its
//! adapter.
//!
//! Payloads carry their last valid block height, so signing needs no network and
//! a pending send can detect expiry. A broadcast persists the signed bytes first.
//! An unresolved send is retried with those exact bytes and never rebuilt with a
//! new blockhash, because both versions could land.

#![deny(clippy::float_arithmetic)]
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
mod identity;
mod keypair;
mod method;
mod node;
mod payload;
mod pending;
mod preview;
mod program;
mod record;
mod rent;
mod route;
mod rpc;
mod send;
mod sign;
mod state;
mod sync;
#[cfg(feature = "test-util")]
pub mod test_util;
mod token;
mod watched;
mod wiring;

pub use crate::adapter::{SolanaAdapter, SolanaAdapterBuilder};
pub use crate::broadcast::BroadcastReply;
pub use crate::config::SolanaConfig;
pub use crate::entry::{ENTRY, SPEC};
pub use crate::error::SolanaError;
pub use crate::identity::SolanaIdentity;
pub use crate::method::Method;
pub use crate::node::{Answered, PooledNode, RpcCall, SolanaNode};
pub use crate::wiring::{build, probe};
