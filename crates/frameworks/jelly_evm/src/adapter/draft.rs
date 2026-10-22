use alloy::consensus::SignableTransaction as _;
use alloy::primitives::{Address, U256};
use jelly_core::{Amount, AssetId, SendAmount, TxRequest, UnsignedTx, duration_millis};
use tokio::join;

use super::EvmAdapter;
use crate::address;
use crate::client::Preflight;
use crate::error::EvmError;
use crate::fee::{FeeMarket, FeePair, GasLimit, GasSource, Overrides};
use crate::ledger::Tracked;
use crate::payload::{self, Shape};
use crate::rpc::CallObject;
use crate::send::{self, Plan};
use crate::token::TokenMeta;

const TARGET: &str = "jelly::evm::adapter";

/// One send as the adapter resolves it, whether the user asked for it or a
/// replacement derived it from the send it replaces.
#[derive(Debug, Clone)]
pub(super) struct Draft<'a> {
    pub(super) from_address_id: i64,
    pub(super) to: &'a str,
    pub(super) asset: AssetId,
    pub(super) amount: SendAmount,
    pub(super) overrides: Overrides,
    pub(super) is_cancel: bool,
}

impl<'a> Draft<'a> {
    pub(super) fn of(request: &'a TxRequest) -> Result<Self, EvmError> {
        Ok(Self {
            from_address_id: request.sole_source().ok_or(EvmError::NotOneSource)?,
            to: &request.to,
            asset: request.asset,
            amount: request.amount,
            overrides: Overrides::of(request)?,
            is_cancel: false,
        })
    }
}

impl EvmAdapter {
    fn sender(&self, address_id: i64) -> Result<Address, EvmError> {
        self.ensure_readable()?;
        self.watch_set()
            .iter()
            .find(|entry| entry.address_id == address_id)
            .map(|entry| entry.owner)
            .ok_or(EvmError::NotWatched { address_id })
    }

    /// Reads the nonce, fees, gas limit and balances, in one round trip where
    /// possible.
    ///
    /// Quotes and builds share this, so the user approves the numbers the build
    /// was priced with, and a price a reused nonce slot would refuse is refused
    /// before it is quoted.
    pub(super) async fn resolve(&self, draft: &Draft<'_>) -> Result<Plan, EvmError> {
        let overrides = &draft.overrides;
        let from = self.sender(draft.from_address_id)?;
        let to = address::parse(self.params.chain, draft.to)?;
        let token = self.tokens.of_asset(draft.asset)?;

        let (read, market) = self.preflight(from, token, overrides.pins_the_fee()).await?;

        let ttl_millis = duration_millis(self.config.reservation_ttl);
        let (nonce, replaces) = {
            let mut ledger = self.ledger.lock();
            ledger.expire_held(self.clock.elapsed_millis(), ttl_millis);
            let nonce = overrides.nonce.unwrap_or_else(|| ledger.next_nonce(from, read.nonce));
            (nonce, ledger.replacement_floor(from, nonce)?)
        };
        // Only a request that pins both halves can lack a market, so the zero
        // default is never read.
        let automatic = market.map_or(FeePair::ZERO, |market| market.level(overrides.level));
        let pair = overrides.pair(automatic, replaces)?;
        if let Some(floor) = replaces
            && !pair.clears(floor)
        {
            return Err(EvmError::ReplacementUnderpriced {
                nonce,
                min_max_fee_per_gas: floor.max_fee_per_gas(),
                min_priority_fee_per_gas: floor.max_priority_fee_per_gas(),
            });
        }

        let calldata = overrides.calldata.clone();
        // A recipient's code can spend more gas on a payment than on nothing, so
        // a native send-max is estimated with the whole balance it spends nearly
        // all of.
        let provisional = match draft.amount {
            SendAmount::Max => {
                token.map_or(read.native_balance_wei, |_| read.token_balance.unwrap_or(0))
            }
            SendAmount::Exact { value } => value.base_units(),
        };
        let shape = Shape::of(token, to, provisional, calldata.as_deref());
        let gas = self.gas_limit(overrides, from, &shape, read.native_balance_wei).await?;
        // The provisional amount is never below the one sent, so the transaction
        // it sizes is never shorter and its fee bounds the real one.
        let data_fee_wei = if self.params.has_l1_data_fee {
            let sized = payload::transaction(self.params, nonce, gas.limit(), pair, &shape);
            self.client.l1_fee_upper_bound(sized.payload_len_for_signature()).await?
        } else {
            0
        };

        // Calldata replaces a native transfer and attaches no coin, so the amount
        // approved, signed and recorded is the zero the transaction carries.
        let is_coinless_call = token.is_none() && calldata.is_some();
        let amount = match draft.amount {
            _ if is_coinless_call => Amount::ZERO,
            SendAmount::Max => send::send_max_amount(
                draft.asset,
                read.native_balance_wei,
                read.token_balance,
                pair,
                gas.limit(),
                data_fee_wei,
            )?,
            SendAmount::Exact { value } => value,
        };

        Ok(Plan {
            params: self.params,
            from,
            to,
            asset: draft.asset,
            token,
            amount,
            should_send_max: draft.amount == SendAmount::Max && !is_coinless_call,
            nonce,
            is_nonce_pinned: overrides.nonce.is_some(),
            market,
            fee: pair,
            gas,
            calldata,
            native_balance_wei: read.native_balance_wei,
            token_balance: read.token_balance,
            replaces,
            is_cancel: draft.is_cancel,
            data_fee_wei,
        })
    }

    /// A pinned price needs the market only for its expected fee, so its fee
    /// history is read beside the batch rather than inside it, and a node that
    /// will not serve the history costs the estimate and nothing else.
    async fn preflight(
        &self,
        from: Address,
        token: Option<TokenMeta>,
        is_fee_pinned: bool,
    ) -> Result<(Preflight, Option<FeeMarket>), EvmError> {
        let blocks = self.params.fee_history_blocks;
        if !is_fee_pinned {
            let read = self.client.preflight(from, token, Some(blocks)).await?;
            let market = read
                .fees
                .as_ref()
                .map(|history| FeeMarket::from_history(history, self.params))
                .transpose()?;
            return Ok((read, market));
        }
        let (read, history) =
            join!(self.client.preflight(from, token, None), self.client.fee_history(blocks));
        let market = history.and_then(|history| FeeMarket::from_history(&history, self.params));
        if let Err(error) = &market {
            tracing::warn!(
                target: TARGET,
                %error,
                "the fee history is unreadable; a pinned price is quoted at its worst case"
            );
        }
        Ok((read?, market.ok()))
    }

    /// The user's limit, else the node's estimate. A call the estimate says reverts
    /// is refused, because it would fail on chain and still pay for its gas.
    ///
    /// The call's value is clamped to the balance, because some providers refuse
    /// to estimate a transfer larger than the sender holds.
    async fn gas_limit(
        &self,
        overrides: &Overrides,
        from: Address,
        shape: &Shape,
        native_balance_wei: u128,
    ) -> Result<GasLimit, EvmError> {
        if let Some(limit) = overrides.gas_limit {
            return GasLimit::new(limit, GasSource::UserOverride);
        }
        let call = CallObject {
            from: Some(from),
            to: shape.to,
            value: (shape.value > 0).then(|| U256::from(shape.value.min(native_balance_wei))),
            input: shape.input.clone(),
        };
        let estimate = self.client.estimate_gas(&call).await.map_err(|error| {
            if matches!(error, EvmError::Reverted { .. }) { EvmError::WouldFail } else { error }
        })?;
        GasLimit::estimated(estimate, &self.config)
    }

    pub(super) async fn prepare(&self, draft: &Draft<'_>) -> Result<UnsignedTx, EvmError> {
        let _building = self.building.lock().await;
        let plan = self.resolve(draft).await?;
        let unsigned = send::build(&plan, &self.tokens)?;
        self.ledger.lock().hold(Tracked::held(
            plan.intent(),
            plan.nonce,
            plan.fee,
            self.clock.elapsed_millis(),
        ));
        Ok(unsigned)
    }
}
