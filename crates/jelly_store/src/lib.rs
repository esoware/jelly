//! `SQLite` storage for wallets and their chain state, addresses, contacts, balances, history,
//! public scan cursors and reservations, endpoints and their health, and TLS pins. Key
//! material and secret Monero state live in the vault, and app settings in the engine's
//! settings file.

#![deny(clippy::float_arithmetic)]

mod address;
mod balance;
mod batch;
mod codec;
mod contact;
mod cursor;
mod db;
mod endpoint;
mod error;
mod patch;
mod pin;
mod schema;
mod sql;
mod store;
mod tx;
mod wallet;

pub use crate::address::{AddressRow, NewAddress};
pub use crate::balance::{AssetTotal, BalanceRow};
pub use crate::batch::SyncBatch;
pub use crate::contact::{ContactEdit, ContactRow, NewContact};
pub use crate::db::Recovered;
pub use crate::error::StoreError;
pub use crate::patch::Patch;
pub use crate::pin::PinRow;
pub use crate::store::{Store, Writer};
pub use crate::tx::{
    PageCursor, TxFilter, TxGroup, TxGroupFilter, TxGroupPage, TxMember, TxPage, TxRow, TxWrite,
};
pub use crate::wallet::{ChainStatePatch, ChainStateRow, NewWallet, WalletRow, WalletSettingsRow};
