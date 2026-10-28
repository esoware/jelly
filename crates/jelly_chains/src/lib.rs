//! The chain registry: the table of chain entries and the adapter build and probe dispatch.
//! It's the only crate that depends on the chain crates, and everything else reaches a
//! chain through [`chains`].
//!
//! A new chain needs an entry in the table and an arm in each match here.

#![deny(clippy::float_arithmetic)]
mod factory;
mod probe;
mod table;

pub use crate::factory::AdapterFactory;
pub use crate::probe::probe;
pub use crate::table::chains;
