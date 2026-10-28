use std::sync::Arc;

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, B256, Bytes, U256};
use jelly_core::ChainId;
use serde::de::IgnoredAny;

use crate::abi::{self, MULTICALL3};
use crate::error::EvmError;
use crate::fee;
use crate::method::Method;
use crate::node::{EvmNode, RpcCall};
use crate::rpc::{BlockTag, CallObject, FeeHistory, LogEntry, LogFilter, Receipt};
use crate::token::TokenMeta;
use crate::units;

const TARGET: &str = "jelly::evm::client";

/// One entry of a balance read, answered in the order it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BalanceRead {
    Native {
        owner: Address,
    },
    Token {
        owner: Address,
        token: TokenMeta,
    },
    /// The owner's transaction count at the latest block, which each of its sends
    /// bumps.
    Nonce {
        owner: Address,
    },
}

/// A second look at sends with no receipt. One node answers the whole batch, so a
/// nonce it says was taken and a receipt it says is missing describe the same chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recheck {
    /// Each owner's account nonce at the block the recheck asked about.
    pub(crate) nonces: Vec<(Address, u64)>,
    pub(crate) sightings: Vec<Sighting>,
}

impl Recheck {
    pub(crate) fn nonce_of(&self, owner: Address) -> Option<u64> {
        self.nonces.iter().find(|(known, _)| *known == owner).map(|(_, nonce)| *nonce)
    }

    pub(crate) fn sighting(&self, txid: B256) -> Option<Sighting> {
        self.sightings.iter().find(|sighting| sighting.txid == txid).copied()
    }
}

/// What the rechecking node knows of one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Sighting {
    pub(crate) txid: B256,
    /// The node has a receipt that puts the transaction in a block.
    pub(crate) has_receipt: bool,
    /// `eth_getTransactionByHash` returned it, from a block or from the mempool.
    pub(crate) is_known: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Preflight {
    /// The next nonce the node will accept, counting this sender's executable
    /// transactions in the node's mempool.
    pub(crate) nonce: u64,
    pub(crate) native_balance_wei: u128,
    pub(crate) token_balance: Option<u128>,
    /// Absent when the caller asked for no fee history.
    pub(crate) fees: Option<FeeHistory>,
}

/// The adapter's HTTP JSON-RPC calls, typed and decoded into this crate's own
/// shapes.
#[derive(Debug)]
pub(crate) struct EvmClient {
    node: Arc<dyn EvmNode>,
    chain: ChainId,
}

impl EvmClient {
    pub(crate) const fn new(node: Arc<dyn EvmNode>, chain: ChainId) -> Self {
        Self { node, chain }
    }

    pub(crate) async fn block_number(&self) -> Result<u64, EvmError> {
        let call = RpcCall::bare(Method::BlockNumber)?;
        let answered = self.node.call(&call).await?;
        units::to_u64("block height", answered.decode(Method::BlockNumber)?)
    }

    pub(crate) async fn estimate_gas(&self, object: &CallObject) -> Result<u64, EvmError> {
        let call = RpcCall::new(Method::EstimateGas, &(object, BlockTag::Latest))?;
        units::to_u64("gas estimate", self.node.call(&call).await?.decode(Method::EstimateGas)?)
    }

    pub(crate) async fn logs(&self, filter: &LogFilter) -> Result<Vec<LogEntry>, EvmError> {
        let call = RpcCall::new(Method::GetLogs, &(filter,))?;
        self.node.call(&call).await?.decode(Method::GetLogs)
    }

    /// `None` where the node has no receipt, which is how both a pending and an
    /// evicted transaction look.
    pub(crate) async fn receipts(&self, txids: &[B256]) -> Result<Vec<Option<Receipt>>, EvmError> {
        if txids.is_empty() {
            return Ok(Vec::new());
        }
        let calls = txids
            .iter()
            .map(|txid| RpcCall::new(Method::GetTransactionReceipt, &(txid,)))
            .collect::<Result<Vec<_>, _>>()?;
        let answered = self.node.batch(&calls).await?;
        answered.check_count(Method::GetTransactionReceipt, txids.len())?;
        (0..txids.len())
            .map(|index| answered.decode_at(index, Method::GetTransactionReceipt))
            .collect()
    }

    /// Every owner's nonce at `at_block`, and for every txid its receipt and whether the
    /// node knows it, in one batch so one node answers all of it.
    pub(crate) async fn recheck(
        &self,
        owners: &[Address],
        at_block: u64,
        txids: &[B256],
    ) -> Result<Recheck, EvmError> {
        let mut calls =
            Vec::with_capacity(owners.len().saturating_add(txids.len().saturating_mul(2)));
        for owner in owners {
            calls.push(RpcCall::new(
                Method::GetTransactionCount,
                &(owner, BlockNumberOrTag::Number(at_block)),
            )?);
        }
        for txid in txids {
            calls.push(RpcCall::new(Method::GetTransactionReceipt, &(txid,))?);
            calls.push(RpcCall::new(Method::GetTransactionByHash, &(txid,))?);
        }
        let answered = self.node.batch(&calls).await?;
        answered.check_count(Method::GetTransactionCount, calls.len())?;
        let mut nonces = Vec::with_capacity(owners.len());
        for (position, owner) in owners.iter().enumerate() {
            let nonce: U256 = answered.decode_at(position, Method::GetTransactionCount)?;
            nonces.push((*owner, units::to_u64("account nonce", nonce)?));
        }
        let mut sightings = Vec::with_capacity(txids.len());
        for (index, txid) in txids.iter().enumerate() {
            let at = owners.len().saturating_add(index.saturating_mul(2));
            let receipt: Option<Receipt> = answered.decode_at(at, Method::GetTransactionReceipt)?;
            let known: Option<IgnoredAny> =
                answered.decode_at(at.saturating_add(1), Method::GetTransactionByHash)?;
            sightings.push(Sighting {
                txid: *txid,
                has_receipt: receipt.is_some_and(|receipt| receipt.landing().is_some()),
                is_known: known.is_some(),
            });
        }
        Ok(Recheck { nonces, sightings })
    }

    /// Tries one Multicall3 `eth_call` first, and falls back to a JSON-RPC batch
    /// for an endpoint that won't serve it.
    pub(crate) async fn balances(
        &self,
        plan: &[BalanceRead],
    ) -> Result<Vec<Option<U256>>, EvmError> {
        if plan.is_empty() {
            return Ok(Vec::new());
        }
        match self.balances_by_multicall(plan).await {
            Ok(rows) => return Ok(rows),
            Err(error) => tracing::debug!(
                target: TARGET,
                %error,
                "the aggregate balance read failed, falling back to a json-rpc batch"
            ),
        }
        self.balances_by_batch(plan).await
    }

    /// A call to an address with no code returns `0x`, so an empty answer is
    /// reported as a missing contract instead of an ABI decode failure.
    ///
    /// No contract can read a nonce, so nonces ride after the aggregate in the same
    /// JSON-RPC batch.
    async fn balances_by_multicall(
        &self,
        plan: &[BalanceRead],
    ) -> Result<Vec<Option<U256>>, EvmError> {
        let mut calls = Vec::with_capacity(plan.len());
        let mut nonces = Vec::new();
        for read in plan {
            match *read {
                BalanceRead::Native { owner } => {
                    calls.push(abi::call3(MULTICALL3, abi::eth_balance_calldata(owner)));
                }
                BalanceRead::Token { owner, token } => {
                    calls.push(abi::call3(token.address(), abi::balance_of_calldata(owner)));
                }
                BalanceRead::Nonce { owner } => nonces
                    .push(RpcCall::new(Method::GetTransactionCount, &(owner, BlockTag::Latest))?),
            }
        }
        let aggregated = calls.len();
        let call = RpcCall::new(
            Method::Call,
            &(CallObject::read(MULTICALL3, abi::aggregate3_calldata(calls)), BlockTag::Latest),
        )?;
        let answered = if nonces.is_empty() {
            self.node.call(&call).await?
        } else {
            let mut batch = Vec::with_capacity(nonces.len().saturating_add(1));
            batch.push(call);
            batch.extend(nonces);
            let answered = self.node.batch(&batch).await?;
            answered.check_count(Method::Call, batch.len())?;
            answered
        };
        let returned: Bytes = answered.decode(Method::Call)?;
        if returned.is_empty() {
            return Err(EvmError::NoContract { address: MULTICALL3, chain: self.chain });
        }
        let results = abi::decode_aggregate3(returned.as_ref())?;
        if results.len() != aggregated {
            return Err(EvmError::answer(
                answered.endpoint(),
                Method::Call,
                format!("an aggregate of {aggregated} answered with {}", results.len()),
            ));
        }
        let mut balances = results.into_iter();
        let mut position = 0usize;
        let mut rows = Vec::with_capacity(plan.len());
        for read in plan {
            let row = match *read {
                BalanceRead::Native { .. } | BalanceRead::Token { .. } => balances
                    .next()
                    .filter(|row| row.success)
                    .and_then(|row| abi::decode_balance(row.returnData.as_ref()).ok()),
                BalanceRead::Nonce { .. } => {
                    position = position.saturating_add(1);
                    answered.decode_at(position, Method::GetTransactionCount).ok()
                }
            };
            rows.push(row);
        }
        Ok(rows)
    }

    async fn balances_by_batch(&self, plan: &[BalanceRead]) -> Result<Vec<Option<U256>>, EvmError> {
        let calls = plan
            .iter()
            .map(|read| match *read {
                BalanceRead::Native { owner } => {
                    RpcCall::new(Method::GetBalance, &(owner, BlockTag::Latest))
                }
                BalanceRead::Token { owner, token } => RpcCall::new(
                    Method::Call,
                    &(
                        CallObject::read(token.address(), abi::balance_of_calldata(owner)),
                        BlockTag::Latest,
                    ),
                ),
                BalanceRead::Nonce { owner } => {
                    RpcCall::new(Method::GetTransactionCount, &(owner, BlockTag::Latest))
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let answered = self.node.batch(&calls).await?;
        answered.check_count(Method::GetBalance, plan.len())?;
        Ok(plan
            .iter()
            .enumerate()
            .map(|(index, read)| match *read {
                BalanceRead::Native { .. } => answered.decode_at(index, Method::GetBalance).ok(),
                BalanceRead::Token { .. } => {
                    let word = answered.decode_at::<Bytes>(index, Method::Call).ok()?;
                    abi::decode_balance(word.as_ref()).ok()
                }
                BalanceRead::Nonce { .. } => {
                    answered.decode_at(index, Method::GetTransactionCount).ok()
                }
            })
            .collect())
    }

    pub(crate) async fn fee_history(&self, blocks: u64) -> Result<FeeHistory, EvmError> {
        let answered = self.node.call(&fee_history_call(blocks)?).await?;
        answered.decode(Method::FeeHistory)
    }

    /// The nonce, the balances, and with `fee_history_blocks` the fee history, in
    /// one batch.
    ///
    /// Public endpoints are paced at one request per 200 ms, so the reads share
    /// one round trip.
    pub(crate) async fn preflight(
        &self,
        owner: Address,
        token: Option<TokenMeta>,
        fee_history_blocks: Option<u64>,
    ) -> Result<Preflight, EvmError> {
        let mut calls = vec![
            RpcCall::new(Method::GetTransactionCount, &(owner, BlockTag::Pending))?,
            RpcCall::new(Method::GetBalance, &(owner, BlockTag::Latest))?,
        ];
        if let Some(metadata) = token {
            calls.push(RpcCall::new(
                Method::Call,
                &(
                    CallObject::read(metadata.address(), abi::balance_of_calldata(owner)),
                    BlockTag::Latest,
                ),
            )?);
        }
        if let Some(blocks) = fee_history_blocks {
            calls.push(fee_history_call(blocks)?);
        }

        let answered = self.node.batch(&calls).await?;
        answered.check_count(Method::GetTransactionCount, calls.len())?;
        let nonce: U256 = answered.decode_at(0, Method::GetTransactionCount)?;
        let native: U256 = answered.decode_at(1, Method::GetBalance)?;
        let mut next = 2usize;
        let token_balance = if token.is_some() {
            let word: Bytes = answered.decode_at(next, Method::Call)?;
            next = next.saturating_add(1);
            Some(units::to_u128("token balance", abi::decode_balance(word.as_ref())?)?)
        } else {
            None
        };
        let history =
            fee_history_blocks.map(|_| answered.decode_at(next, Method::FeeHistory)).transpose()?;
        Ok(Preflight {
            nonce: units::to_u64("nonce", nonce)?,
            native_balance_wei: units::to_u128("native balance", native)?,
            token_balance,
            fees: history,
        })
    }

    /// The most the OP Stack's L1 data fee can be for an unsigned transaction of
    /// `unsigned_tx_size` bytes.
    pub(crate) async fn l1_fee_upper_bound(
        &self,
        unsigned_tx_size: usize,
    ) -> Result<u128, EvmError> {
        let calldata = abi::l1_fee_upper_bound_calldata(unsigned_tx_size);
        let call = RpcCall::new(
            Method::Call,
            &(CallObject::read(abi::GAS_PRICE_ORACLE, calldata), BlockTag::Latest),
        )?;
        let returned: Bytes = self.node.call(&call).await?.decode(Method::Call)?;
        if returned.is_empty() {
            return Err(EvmError::NoContract { address: abi::GAS_PRICE_ORACLE, chain: self.chain });
        }
        units::to_u128("L1 data fee", abi::decode_l1_fee(returned.as_ref())?)
    }

    /// One attempt against one endpoint.
    pub(crate) async fn send_raw(&self, raw: &[u8]) -> Result<B256, EvmError> {
        let call = RpcCall::new(Method::SendRawTransaction, &(Bytes::copy_from_slice(raw),))?;
        self.node.broadcast(&call).await?.decode(Method::SendRawTransaction)
    }
}

fn fee_history_call(blocks: u64) -> Result<RpcCall, EvmError> {
    RpcCall::new(
        Method::FeeHistory,
        &(U256::from(blocks.max(1)), BlockTag::Latest, fee::REWARD_PERCENTILES),
    )
}
