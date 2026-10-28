//! Wallet session and sync engine. One engine owns a data directory and at most one
//! unlocked session. It builds adapters, runs a sync loop per chain, and persists their
//! events.
//!
//! Only the engine holds the database handle, endpoint and TLS pin tables included, so
//! only a surviving engine clone can keep shutdown from checkpointing it.

#![deny(clippy::float_arithmetic)]

mod address;
mod balance;
mod chains;
mod config;
mod contact;
mod cursor;
mod derive;
mod endpoint;
mod engine;
mod error;
mod events;
mod export;
#[cfg(test)]
mod fixtures;
mod history;
mod pending;
mod pin;
mod pump;
mod secret_cursor;
mod seed;
mod session;
mod settings;
mod status;
mod sync;
#[cfg(feature = "test-util")]
pub mod test_util;
mod tx;
mod wallet;

pub use crate::address::AddressRecord;
pub use crate::balance::{AssetTotal, BalanceRecord};
pub use crate::config::EngineConfig;
pub use crate::contact::ContactRecord;
pub use crate::engine::{DatabaseRebuilt, EngineDeps, StartFailure, Started, WalletEngine};
pub use crate::error::StartupError;
pub use crate::history::{TxEntry, TxPage, TxQuery};
pub use crate::pending::{PreparedTx, PreviewDigest};
pub use crate::session::{LockReason, LockState};
pub use crate::settings::{AmountEntry, AppSettings, BackdropSetting};
pub use crate::wallet::{CreatedWallet, PassphraseInput, WalletRecord};
