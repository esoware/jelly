//! BIP-39 phrases and their seed expansion. Only the engine depends on this crate, so no
//! chain code can name a phrase.

#![deny(clippy::float_arithmetic)]
mod mnemonic;

pub use crate::mnemonic::Mnemonic;
