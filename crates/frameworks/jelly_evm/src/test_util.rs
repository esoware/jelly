//! Scripted EVM services and the chain crates' address vectors for tests. Only
//! `[dev-dependencies]` enable the `test-util` feature.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alloy::primitives::Address;
use async_trait::async_trait;
use jelly_core::EndpointLabel;
use jelly_net::NetError;
use parking_lot::Mutex;
use serde::Deserialize;

use crate::error::EvmError;
use crate::etherscan;
use crate::history::{HistoryApi, HistoryPage, InternalRow, NativeRow, TokenRow};

/// The address the all-abandon test phrase derives at `m/44'/60'/0'/0/0`. Every EVM
/// network shares that path, so each chain crate's vector secret holds this address.
pub const PAYEE: &str = "0x9858EfFD232B4033E47d90003D41EC34EcaEda94";

/// A chain crate's `tests/vectors/addresses.json`.
#[derive(Debug, Deserialize)]
pub struct Vectors {
    pub phrase: String,
    pub secret_path: String,
    pub secret: String,
    pub derived: Vec<Derived>,
}

#[derive(Debug, Deserialize)]
pub struct Derived {
    pub path: String,
    pub address: String,
}

/// The endpoint every scripted service answers as.
pub fn endpoint() -> EndpointLabel {
    EndpointLabel::new("https", "node.invalid", None)
}

#[derive(Debug, Default)]
pub struct FakeHistory {
    native: Mutex<Vec<NativeRow>>,
    tokens: Mutex<Vec<TokenRow>>,
    internal: Mutex<Vec<InternalRow>>,
    is_truncated: AtomicBool,
    is_failing: AtomicBool,
    refusal: Mutex<Option<String>>,
    reads: Mutex<Vec<(Address, u64)>>,
    internal_reads: Mutex<Vec<(Address, u64)>>,
}

impl FakeHistory {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Loads pages from recorded Etherscan bodies through the crate's own reader,
    /// so the fixture drives the shipped decoder.
    pub fn serve(&self, native_body: &str, token_body: &str) -> Result<(), EvmError> {
        *self.native.lock() = etherscan::read_page(native_body)?.rows;
        *self.tokens.lock() = etherscan::read_page(token_body)?.rows;
        Ok(())
    }

    /// Loads the internal calls from a recorded body the same way.
    pub fn serve_internal(&self, body: &str) -> Result<(), EvmError> {
        *self.internal.lock() = etherscan::read_page(body)?.rows;
        Ok(())
    }

    pub fn truncate(&self) -> &Self {
        self.is_truncated.store(true, Ordering::SeqCst);
        self
    }

    pub fn fail(&self) -> &Self {
        self.is_failing.store(true, Ordering::SeqCst);
        self
    }

    /// Answers every read with the refusal a recorded body carries.
    pub fn refuse(&self, body: &str) -> &Self {
        *self.refusal.lock() = Some(body.to_owned());
        self
    }

    /// The `(address, start block)` pairs the adapter asked for native and token
    /// transfers.
    pub fn reads(&self) -> Vec<(Address, u64)> {
        self.reads.lock().clone()
    }

    /// The `(address, start block)` pairs the adapter asked for internal calls.
    pub fn internal_reads(&self) -> Vec<(Address, u64)> {
        self.internal_reads.lock().clone()
    }

    fn read(&self, owner: Address, start_block: u64) -> Result<(), EvmError> {
        self.reads.lock().push((owner, start_block));
        self.answer()
    }

    fn answer(&self) -> Result<(), EvmError> {
        if self.is_failing.load(Ordering::SeqCst) {
            return Err(EvmError::Net(NetError::protocol(&endpoint(), "down")));
        }
        if let Some(body) = self.refusal.lock().as_deref() {
            etherscan::read_page::<NativeRow>(body)?;
        }
        Ok(())
    }
}

#[async_trait]
impl HistoryApi for FakeHistory {
    async fn native(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<NativeRow>, EvmError> {
        self.read(address, start_block)?;
        Ok(HistoryPage::of(self.native.lock().clone(), self.is_truncated.load(Ordering::SeqCst)))
    }

    async fn tokens(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<TokenRow>, EvmError> {
        self.read(address, start_block)?;
        Ok(HistoryPage::of(self.tokens.lock().clone(), self.is_truncated.load(Ordering::SeqCst)))
    }

    /// Never truncated, so the other two lists alone decide how far a truncated
    /// read reaches.
    async fn internal(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<InternalRow>, EvmError> {
        self.internal_reads.lock().push((address, start_block));
        self.answer()?;
        Ok(HistoryPage::of(self.internal.lock().clone(), false))
    }
}
