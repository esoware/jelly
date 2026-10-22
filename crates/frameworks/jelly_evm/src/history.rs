use std::fmt;

use alloy::primitives::{Address, B256};
use async_trait::async_trait;
use serde::Deserialize;

use crate::error::EvmError;

/// A transfer history service.
///
/// Without one, native transfers leave no log to find and sync falls back to
/// watching the balance.
#[async_trait]
pub trait HistoryApi: Send + Sync + fmt::Debug {
    /// Normal transactions sent from or to `address`, at or after `start_block`.
    async fn native(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<NativeRow>, EvmError>;

    /// Token transfers of `address` at or after `start_block`.
    async fn tokens(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<TokenRow>, EvmError>;

    /// Native coin that contract calls moved to or from `address`, at or after
    /// `start_block`.
    async fn internal(
        &self,
        address: Address,
        start_block: u64,
    ) -> Result<HistoryPage<InternalRow>, EvmError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryPage<T> {
    rows: Vec<T>,
    is_truncated: bool,
}

impl<T> HistoryPage<T> {
    pub const fn of(rows: Vec<T>, is_truncated: bool) -> Self {
        Self { rows, is_truncated }
    }

    pub fn rows(&self) -> &[T] {
        &self.rows
    }

    /// True when the service stopped short of the whole history, so the frontier
    /// may only move as far as the rows reach.
    pub const fn is_truncated(&self) -> bool {
        self.is_truncated
    }

    /// Derived from the rows, so every [`HistoryApi`] reports the same resume
    /// block for the same rows.
    pub(crate) fn highest_block(&self) -> Option<u64>
    where
        T: HistoryRow,
    {
        self.rows.iter().map(HistoryRow::block).max()
    }
}

pub(crate) trait HistoryRow {
    fn block(&self) -> u64;
}

/// One normal (externally sent) transaction as `txlist` reports it, including
/// zero-value contract calls. Numbers arrive as decimal strings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeRow {
    #[serde(deserialize_with = "text::decimal")]
    pub block_number: u64,
    #[serde(rename = "timeStamp", deserialize_with = "text::decimal")]
    pub timestamp: i64,
    pub hash: B256,
    pub from: Address,
    /// Absent on a contract creation, which pays nobody.
    #[serde(default, deserialize_with = "text::address")]
    pub to: Option<Address>,
    #[serde(deserialize_with = "text::decimal")]
    pub value: u128,
    #[serde(default, deserialize_with = "text::optional_decimal")]
    pub gas_used: Option<u64>,
    #[serde(default, deserialize_with = "text::optional_decimal")]
    pub gas_price: Option<u128>,
    /// `1` when execution failed.
    #[serde(rename = "isError", default, deserialize_with = "text::flag")]
    pub is_failed: bool,
}

impl HistoryRow for NativeRow {
    fn block(&self) -> u64 {
        self.block_number
    }
}

/// One token transfer as the `tokentx` action reports it.
///
/// `tokenDecimal` is ignored, because decimals come from the compiled-in
/// registry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRow {
    #[serde(deserialize_with = "text::decimal")]
    pub block_number: u64,
    #[serde(rename = "timeStamp", deserialize_with = "text::decimal")]
    pub timestamp: i64,
    pub hash: B256,
    pub from: Address,
    pub to: Address,
    pub contract_address: Address,
    #[serde(deserialize_with = "text::decimal")]
    pub value: u128,
    #[serde(default, deserialize_with = "text::optional_decimal")]
    pub gas_used: Option<u64>,
    #[serde(default, deserialize_with = "text::optional_decimal")]
    pub gas_price: Option<u128>,
}

impl HistoryRow for TokenRow {
    fn block(&self) -> u64 {
        self.block_number
    }
}

/// One call inside a transaction as the `txlistinternal` action reports it.
///
/// It carries no fee, because only the transaction's sender pays for gas, and
/// that is on the transaction's own row.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternalRow {
    #[serde(deserialize_with = "text::decimal")]
    pub block_number: u64,
    #[serde(rename = "timeStamp", deserialize_with = "text::decimal")]
    pub timestamp: i64,
    /// The hash of the transaction the call ran in, which that transaction's own
    /// row shares.
    pub hash: B256,
    pub from: Address,
    /// Absent when the call created a contract.
    #[serde(default, deserialize_with = "text::address")]
    pub to: Option<Address>,
    #[serde(deserialize_with = "text::decimal")]
    pub value: u128,
    #[serde(rename = "isError", default, deserialize_with = "text::flag")]
    pub is_failed: bool,
    /// A delegate call, a static call or a call code runs in its caller's own
    /// context and moves no coin, whatever value the row reports.
    #[serde(rename = "type", default, deserialize_with = "text::context_call")]
    pub is_context_call: bool,
}

impl HistoryRow for InternalRow {
    fn block(&self) -> u64 {
        self.block_number
    }
}

/// Deserialisers for the string-encoded fields the service sends.
mod text {
    use std::str::FromStr;

    use alloy::primitives::Address;
    use serde::de::Error as _;
    use serde::{Deserialize as _, Deserializer};

    pub(super) fn decimal<'de, D: Deserializer<'de>, T: FromStr>(
        deserializer: D,
    ) -> Result<T, D::Error> {
        let raw = <&str>::deserialize(deserializer)?;
        raw.trim().parse().map_err(|_| D::Error::custom("not a decimal integer"))
    }

    pub(super) fn optional_decimal<'de, D: Deserializer<'de>, T: FromStr>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        let raw = Option::<&str>::deserialize(deserializer)?;
        Ok(raw.and_then(|raw| raw.trim().parse().ok()))
    }

    pub(super) fn flag<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
        let raw = <&str>::deserialize(deserializer)?;
        Ok(raw.trim() == "1")
    }

    pub(super) fn context_call<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<bool, D::Error> {
        let raw = <&str>::deserialize(deserializer)?;
        let kind = raw.trim();
        Ok(["delegatecall", "staticcall", "callcode"]
            .iter()
            .any(|call| kind.eq_ignore_ascii_case(call)))
    }

    /// An empty address member means there was no recipient.
    pub(super) fn address<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Address>, D::Error> {
        let raw = <&str>::deserialize(deserializer)?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        trimmed.parse().map(Some).map_err(|_| D::Error::custom("not an address"))
    }
}
