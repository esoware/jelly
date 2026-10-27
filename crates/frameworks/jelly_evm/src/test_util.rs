//! Scripted EVM services, network figures, and the chain crates' address vectors for
//! tests. Only `[dev-dependencies]` enable the `test-util` feature.
//!
//! The networks' params here are copies of the chain crates' own, for this crate's
//! unit tests. Those cannot take them from a chain crate, whose copy of this crate is a
//! separate build with its own types. Each chain crate's tests assert that its params
//! match its copy.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use alloy::primitives::Address;
use async_trait::async_trait;
use jelly_core::{ChainId, EndpointLabel, TokenId};
use jelly_net::NetError;
use parking_lot::Mutex;
use serde::Deserialize;

use crate::error::EvmError;
use crate::etherscan;
use crate::history::{HistoryApi, HistoryPage, InternalRow, NativeRow, TokenRow};
use crate::params::EvmParams;

/// The address the all-abandon test phrase derives at `m/44'/60'/0'/0/0`, which every
/// EVM network shares, so it is the address each chain crate's vector secret holds.
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

/// The Ethereum figures the chain crate ships.
pub const ETHEREUM: EvmParams = EvmParams {
    chain: ChainId::Ethereum,
    chain_id: 1,
    request_timeout: Duration::from_secs(10),
    tokens: &[
        TokenId::LinkErc20,
        TokenId::WbtcErc20,
        TokenId::WethErc20,
        TokenId::UniErc20,
        TokenId::ShibErc20,
        TokenId::EnaErc20,
        TokenId::AaveErc20,
        TokenId::OndoErc20,
        TokenId::PepeErc20,
        TokenId::ArbErc20,
        TokenId::UsdtErc20,
        TokenId::UsdcErc20,
        TokenId::UsdsErc20,
        TokenId::DaiErc20,
        TokenId::UsdgErc20,
        TokenId::PyusdErc20,
        TokenId::XautErc20,
        TokenId::PaxgErc20,
    ],
    token_standard: "ERC-20",
    block_millis: 12_000,
    watch_floor: Duration::from_secs(5),
    fee_history_blocks: 20,
    min_priority_fee_wei: 100_000_000,
    max_priority_fee_wei: None,
    confirmations_target: 12,
    log_window_blocks: 1_000,
    backfill_floor: 4_281_611,
    has_native_transfer_logs: false,
    has_l1_data_fee: false,
    has_keyless_history: true,
};

/// The Arbitrum One figures the chain crate ships.
pub const ARBITRUM: EvmParams = EvmParams {
    chain: ChainId::Arbitrum,
    chain_id: 42_161,
    request_timeout: Duration::from_secs(10),
    tokens: &[
        TokenId::LinkArbitrum,
        TokenId::WethArbitrum,
        TokenId::ArbArbitrum,
        TokenId::UsdtArbitrum,
        TokenId::UsdcArbitrum,
        TokenId::UsdsArbitrum,
        TokenId::UsdgArbitrum,
        TokenId::PyusdArbitrum,
        TokenId::XautArbitrum,
    ],
    token_standard: "ERC-20",
    block_millis: 250,
    watch_floor: Duration::from_secs(6),
    fee_history_blocks: 20,
    min_priority_fee_wei: 0,
    max_priority_fee_wei: None,
    confirmations_target: 20,
    log_window_blocks: 50_000,
    backfill_floor: 0,
    has_native_transfer_logs: false,
    has_l1_data_fee: false,
    has_keyless_history: false,
};

/// The Base figures the chain crate ships.
pub const BASE: EvmParams = EvmParams {
    chain: ChainId::Base,
    chain_id: 8_453,
    request_timeout: Duration::from_secs(10),
    tokens: &[TokenId::LinkBase, TokenId::WethBase, TokenId::UsdcBase, TokenId::UsdsBase],
    token_standard: "ERC-20",
    block_millis: 2_000,
    watch_floor: Duration::from_secs(6),
    fee_history_blocks: 30,
    min_priority_fee_wei: 1_000_000,
    max_priority_fee_wei: None,
    confirmations_target: 10,
    log_window_blocks: 10_000,
    backfill_floor: 0,
    has_native_transfer_logs: false,
    has_l1_data_fee: true,
    has_keyless_history: false,
};

/// The OP Mainnet figures the chain crate ships.
pub const OPTIMISM: EvmParams = EvmParams {
    chain: ChainId::Optimism,
    chain_id: 10,
    request_timeout: Duration::from_secs(10),
    tokens: &[
        TokenId::LinkOptimism,
        TokenId::WethOptimism,
        TokenId::OpOptimism,
        TokenId::UsdtOptimism,
        TokenId::UsdcOptimism,
        TokenId::UsdsOptimism,
    ],
    token_standard: "ERC-20",
    block_millis: 2_000,
    watch_floor: Duration::from_secs(6),
    fee_history_blocks: 30,
    min_priority_fee_wei: 1_000_000,
    max_priority_fee_wei: None,
    confirmations_target: 10,
    log_window_blocks: 10_000,
    backfill_floor: 0,
    has_native_transfer_logs: false,
    has_l1_data_fee: true,
    has_keyless_history: false,
};

/// The BNB Smart Chain figures the chain crate ships.
pub const BSC: EvmParams = EvmParams {
    chain: ChainId::Bsc,
    chain_id: 56,
    request_timeout: Duration::from_secs(10),
    tokens: &[
        TokenId::LinkBep20,
        TokenId::WethBep20,
        TokenId::UsdtBep20,
        TokenId::UsdcBep20,
        TokenId::XautBep20,
    ],
    token_standard: "BEP-20",
    block_millis: 450,
    watch_floor: Duration::from_secs(6),
    fee_history_blocks: 100,
    min_priority_fee_wei: 50_000_000,
    max_priority_fee_wei: None,
    confirmations_target: 15,
    log_window_blocks: 1_000,
    backfill_floor: 176_416,
    has_native_transfer_logs: false,
    has_l1_data_fee: false,
    has_keyless_history: false,
};

/// The Polygon figures the chain crate ships.
pub const POLYGON: EvmParams = EvmParams {
    chain: ChainId::Polygon,
    chain_id: 137,
    request_timeout: Duration::from_secs(10),
    tokens: &[
        TokenId::LinkPolygon,
        TokenId::WethPolygon,
        TokenId::UsdtPolygon,
        TokenId::UsdcPolygon,
        TokenId::PyusdPolygon,
        TokenId::XautPolygon,
    ],
    token_standard: "ERC-20",
    block_millis: 1_500,
    watch_floor: Duration::from_secs(6),
    fee_history_blocks: 40,
    min_priority_fee_wei: 25_000_000_000,
    max_priority_fee_wei: None,
    confirmations_target: 16,
    log_window_blocks: 10_000,
    backfill_floor: 0,
    has_native_transfer_logs: true,
    has_l1_data_fee: false,
    has_keyless_history: false,
};

/// The Avalanche C-Chain figures the chain crate ships.
pub const AVALANCHE: EvmParams = EvmParams {
    chain: ChainId::Avalanche,
    chain_id: 43_114,
    request_timeout: Duration::from_secs(10),
    tokens: &[
        TokenId::LinkAvalanche,
        TokenId::WethAvalanche,
        TokenId::UsdtAvalanche,
        TokenId::UsdcAvalanche,
        TokenId::UsdsAvalanche,
        TokenId::XautAvalanche,
    ],
    token_standard: "ERC-20",
    block_millis: 1_000,
    watch_floor: Duration::from_secs(6),
    fee_history_blocks: 60,
    min_priority_fee_wei: 0,
    max_priority_fee_wei: Some(150),
    confirmations_target: 10,
    log_window_blocks: 10_000,
    backfill_floor: 2_749_890,
    has_native_transfer_logs: false,
    has_l1_data_fee: false,
    has_keyless_history: true,
};
