//! Every request this crate makes, typed once.
//!
//! TRON's HTTP API has no batch form, so each method is one request and one
//! decode, except [`TronClient::parent`], which takes at least two. A TRX quote
//! costs four round trips and a token quote five. A pass costs one per address for
//! the TRX balance plus one per token. When an address's history is due, it also
//! costs one per history page, one receipt per TRC-20 row whose block the pass
//! hasn't seen, and one parent lookup per internal transfer the native stream
//! hasn't already reported. So nothing here fetches a value it can derive.

use std::sync::Arc;

use jelly_core::{Clock, Deadline, TokenId};
use serde::de::DeserializeOwned;

use crate::abi;
use crate::address;
use crate::cursor::Stream;
use crate::error::TronError;
use crate::fee::EnergyReading;
use crate::history::{NativeRow, Page, Row as _};
use crate::node::{Answered, TronNode};
use crate::params::ChainParams;
use crate::route::Request;
use crate::shape::{
    Account, BlockRef, ChainParameters, ConstantCall, NowBlock, Resources, TransactionInfo,
    TransactionRow,
};

/// The most pages of a sender's history at one block time that a parent lookup
/// reads.
///
/// That's 1000 rows, far more than one account sends in a block, so the bound only
/// stops an indexer whose fingerprints never run out.
const PARENT_PAGES: u32 = 5;

#[derive(Debug)]
pub(crate) struct TronClient {
    node: Arc<dyn TronNode>,
}

impl TronClient {
    pub(crate) const fn new(node: Arc<dyn TronNode>) -> Self {
        Self { node }
    }

    /// The head block: the tip, and the TAPOS reference every transaction pins
    /// itself to.
    pub(crate) async fn now_block(&self) -> Result<BlockRef, TronError> {
        let answer = self.node.call(&Request::now_block()?).await?;
        let block: NowBlock = answer.decode()?;
        block.block_ref(&answer)
    }

    /// The governance parameters that set fee prices.
    pub(crate) async fn chain_parameters(&self) -> Result<ChainParams, TronError> {
        let reported: ChainParameters =
            self.node.call(&Request::chain_parameters()?).await?.decode()?;
        Ok(ChainParams::from(&reported))
    }

    /// The TRX balance and whether the account exists at all.
    pub(crate) async fn account(&self, address: &str) -> Result<Account, TronError> {
        self.node.call(&Request::account(address)?).await?.decode()
    }

    /// One account's bandwidth and energy pools.
    pub(crate) async fn resources(&self, address: &str) -> Result<Resources, TronError> {
        self.node.call(&Request::account_resource(address)?).await?.decode()
    }

    /// A TRC-20 balance, from `balanceOf(owner)`.
    pub(crate) async fn token_balance(
        &self,
        owner: &str,
        owner_body: &[u8; address::BODY_BYTES],
        token: TokenId,
    ) -> Result<u128, TronError> {
        let request = Request::balance_of(owner, owner_body, token.contract())?;
        let answer = self.node.call(&request).await?;
        let call: ConstantCall = answer.decode()?;
        if let Some(detail) = call.failure() {
            return Err(TronError::CallFailed { detail });
        }
        let word = call.first_word().ok_or_else(|| answer.unusable("return word"))?;
        abi::decode_balance(word).ok_or_else(|| answer.unusable("return word"))
    }

    /// The energy `transfer(to, amount)` would use, measured by the node running
    /// the call read-only.
    ///
    /// `energy_used` already includes `energy_penalty`, the dynamic surcharge on a
    /// busy contract, so don't add them together.
    pub(crate) async fn measure_transfer(
        &self,
        owner: &str,
        token: TokenId,
        to: &[u8; address::BODY_BYTES],
        amount: u128,
    ) -> Result<EnergyReading, TronError> {
        let request = Request::measure_transfer(owner, token.contract(), to, amount)?;
        let answer = self.node.call(&request).await?;
        let call: ConstantCall = answer.decode()?;
        if let Some(detail) = call.failure() {
            return Err(TronError::CallFailed { detail });
        }
        Ok(EnergyReading { needed: call.energy_used(), penalty: call.energy_penalty() })
    }

    /// A mined transaction's receipt, with its block, its result, and the TRX it
    /// burned.
    pub(crate) async fn transaction_info(&self, txid: &str) -> Result<TransactionInfo, TronError> {
        self.node.call(&Request::transaction_info_by_id(txid)?).await?.decode()
    }

    /// Whether the network holds a transaction at all, pool included, which tells
    /// an unlanded send that's in flight from a dead one.
    pub(crate) async fn holds_transaction(&self, txid: &str) -> Result<bool, TronError> {
        let row: TransactionRow =
            self.node.call(&Request::transaction_by_id(txid)?).await?.decode()?;
        Ok(row.is_found())
    }

    /// One page of an indexer history stream, newest first.
    pub(crate) async fn history<T: DeserializeOwned>(
        &self,
        stream: Stream,
        address: &str,
        fingerprint: Option<&str>,
    ) -> Result<Page<T>, TronError> {
        let request = Request::history(stream, address, fingerprint);
        self.node.call(&request).await?.decode()
    }

    /// The indexer row of the transaction an internal transfer ran in. It's found
    /// in the sender's history at the transfer's block time, so it's the same row,
    /// internal transfers and all, that the sender's native stream lists.
    ///
    /// The sender comes from `gettransactionbyid`, which every full node answers
    /// alike. A row not found within [`PARENT_PAGES`] pages and `deadline` is
    /// reported as unusable rather than guessed at.
    pub(crate) async fn parent(
        &self,
        txid: &str,
        timestamp_millis: i64,
        deadline: Deadline,
        clock: &dyn Clock,
    ) -> Result<NativeRow, TronError> {
        let answer = self.node.call(&Request::transaction_by_id(txid)?).await?;
        let held: NativeRow = answer.decode()?;
        let sender = held.sender().ok_or_else(|| answer.unusable("transaction sender"))?;
        let mut fingerprint = None;
        let mut pages_read: u32 = 0;
        loop {
            let request = Request::sent_at(&sender, timestamp_millis, fingerprint.as_deref());
            let answer = self.node.call(&request).await?;
            let page: Page<NativeRow> = answer.decode()?;
            pages_read = pages_read.saturating_add(1);
            let next = page.fingerprint().map(str::to_owned);
            let has_ended = page.data.is_empty() || next.is_none() || next == fingerprint;
            if let Some(row) = page.data.into_iter().find(|row| row.txid() == txid) {
                return Ok(row);
            }
            if has_ended || pages_read >= PARENT_PAGES || deadline.is_expired(clock) {
                return Err(answer.unusable("parent transaction"));
            }
            fingerprint = next;
        }
    }

    /// Sends the signed bytes to exactly one endpoint, once.
    pub(crate) async fn broadcast(
        &self,
        signed: &[u8],
        json_body: String,
    ) -> Result<Answered, TronError> {
        let preferred = Request::broadcast_hex(signed)?;
        let fallback = Request::broadcast_json(json_body);
        self.node.broadcast(&preferred, &fallback).await
    }
}
