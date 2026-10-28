//! Solana mainnet-beta.
//!
//! A send is persisted before broadcast and only ever resent as the same signed bytes,
//! because a rebuild with a fresh blockhash could land alongside the original.

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
