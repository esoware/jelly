//! The chain registry. This is the only crate that names every chain crate, so the one
//! table of entries, the one adapter build dispatch, and the one probe dispatch live here.
//!
//! Everything else reaches a chain through [`chains`] by [`jelly_core::ChainId`], and
//! adding a chain is one variant, one chain crate, one entry in the table, and one arm
//! in each match here.

#![deny(clippy::float_arithmetic)]
mod factory;
mod probe;
mod table;

pub use crate::factory::AdapterFactory;
pub use crate::probe::probe;
pub use crate::table::chains;
