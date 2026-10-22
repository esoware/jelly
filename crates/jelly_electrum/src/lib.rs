//! Async Electrum client for the Bitcoin-derived chains. Every endpoint uses TLS
//! and must prove its chain by hashing the blocks the caller names.
//!
//! Subscriptions only wake a sync, which re-fetches everything it uses.

#![deny(clippy::float_arithmetic)]
mod chain;
mod client;
mod connection;
mod connector;
mod error;
mod fee;
mod frame;
mod header;
mod history;
mod method;
mod reject;
mod rpc_socket;
mod script_hash;
#[cfg(feature = "test-util")]
pub mod test_util;
mod tx;
mod unspent;
mod version;

pub use crate::chain::{Checkpoint, ElectrumChain};
pub use crate::client::ElectrumClient;
pub use crate::connector::ElectrumConnector;
pub use crate::error::{ElectrumError, classify, classify_broadcast};
pub use crate::fee::{EstimationMode, FeeReading, HistogramBucket, MempoolInfo, read_sat_per_vb};
pub use crate::history::{Confirmation, HistoryDigest, HistoryEntry};
pub use crate::method::Method;
pub use crate::tx::ServedTx;
pub use crate::unspent::Unspent;
pub use crate::version::ProtocolVersion;
