use jelly_core::{Amount, Clock, FeeLevel, FeeQuote, FeeRate, FeeSelection, LevelFee};
use jelly_electrum::EstimationMode;
use tokio::sync::Mutex;

use crate::adapter::client::UtxoClient;
use crate::adapter::send::{self, Plan};
use crate::error::UtxoError;
use crate::fee::{fee_for, rate_from_histogram};
use crate::params::{Auction, BumpMethod, FeeMarket, UtxoParams};
use crate::weight::predict_weight;

const TARGET: &str = "jelly::utxo::fee";

/// Estimates update per block, so send-form keystrokes reuse a reading.
const FEE_CACHE_TTL_MILLIS: u64 = 30_000;

#[derive(Debug, Clone, Copy)]
pub(crate) struct FeeContext {
    /// The rate each fee level pays, in [`FeeLevel::ALL`] order, never below the relay
    /// floor or the table's wallet floor, and never below a slower level's.
    levels: [FeeRate; 3],
    /// The slow reading floored only at the relay floor, so a custom rate is judged
    /// slow against what the network takes, not what this wallet pays.
    pub(crate) slow_market: FeeRate,
    /// The compiled minimum relay fee, or the network's reported floor when it's
    /// higher.
    pub(crate) relay_floor: FeeRate,
    /// The table the reading was taken for, with any higher incremental relay fee a
    /// node reported folded in, since a replacement has to clear what the node
    /// charges.
    pub(crate) params: UtxoParams,
}

impl FeeContext {
    /// `market` is each level's reading in [`FeeLevel::ALL`] order. A reading that ranks
    /// a faster level below a slower one is raised, so a faster level never pays less.
    fn settle(market: [FeeRate; 3], relay_floor: FeeRate, params: &UtxoParams) -> Self {
        let [slow_market, normal_market, fast_market] = market.map(|rate| rate.max(relay_floor));
        let slow = slow_market.max(params.min_tx_fee);
        let normal = normal_market.max(slow);
        let fast = fast_market.max(normal);
        Self { levels: [slow, normal, fast], slow_market, relay_floor, params: *params }
    }

    pub(crate) const fn level(&self, level: FeeLevel) -> FeeRate {
        let [slow, normal, fast] = self.levels;
        match level {
            FeeLevel::Slow => slow,
            FeeLevel::Normal => normal,
            FeeLevel::Fast => fast,
        }
    }
}

/// The table's own rate at every level, with no server asked. That rate is also the
/// market, so a custom rate is judged slow against what the reference wallet pays.
fn flat(params: &UtxoParams) -> FeeContext {
    let relay_floor = params.min_relay_fee;
    FeeContext::settle([params.min_tx_fee; 3], relay_floor, params)
}

/// Tries the fee API, then the Electrum estimator, then the mempool histogram,
/// and falls back to the relay floor.
///
/// Protocol 1.6 servers report the relay and incremental fees through
/// `mempool.get_info`. For older servers the relay floor comes from the fee
/// service.
async fn bid(params: &UtxoParams, auction: &Auction, client: &UtxoClient) -> FeeContext {
    let chain = params.chain.as_str();
    let observed = match client.mempool_info().await {
        Ok(info) => Some(info),
        Err(error) => {
            tracing::debug!(target: TARGET, chain, %error, "no relay floor from the server");
            None
        }
    };
    let recommended = match client.fee_api() {
        None => None,
        Some(api) => match api.recommended().await {
            Ok(fees) => Some(fees),
            Err(error) => {
                tracing::debug!(target: TARGET, chain, %error, "the fee service didn't answer");
                None
            }
        },
    };

    // A send is broadcast to this server, so if it answered without a floor, the
    // compiled minimum beats the fee service's view of some other node.
    let floor = observed
        .map(|info| info.relay_floor().unwrap_or(FeeRate::ZERO))
        .or_else(|| recommended?.minimum);
    let relay_floor = params.relay_floor(floor);
    let table =
        params.with_incremental_relay_fee(observed.and_then(|info| info.incremental_relay_fee));
    let settle = |market: [FeeRate; 3]| FeeContext::settle(market, relay_floor, &table);

    if let Some(fees) = recommended {
        return settle([fees.hour, fees.half_hour, fees.fastest]);
    }

    // Economical estimates follow the mempool down, and only a replaceable send can
    // recover from a low guess. Bitcoin Core's wallet estimates economically only
    // when it signals replaceability, and Litecoin Core's, which doesn't signal,
    // estimates conservatively.
    let mode = if params.bump_method == Some(BumpMethod::Replace) {
        EstimationMode::Economical
    } else {
        EstimationMode::Conservative
    };
    let mut estimates = [None; 3];
    for (slot, target_blocks) in estimates.iter_mut().zip(auction.target_blocks) {
        match client.estimate_fee(target_blocks, mode).await {
            Ok(Some(rate)) => *slot = Some(rate),
            Ok(None) => {
                tracing::debug!(target: TARGET, chain, target_blocks, "the server has no estimate");
                break;
            }
            Err(error) => {
                tracing::debug!(target: TARGET, chain, %error, "no estimate from the server");
                break;
            }
        }
    }
    if let [Some(slow), Some(normal), Some(fast)] = estimates {
        return settle([slow, normal, fast]);
    }

    match client.fee_histogram().await {
        Ok(buckets) => {
            let read = auction
                .target_blocks
                .map(|target_blocks| rate_from_histogram(auction, target_blocks, &buckets));
            if let [Some(slow), Some(normal), Some(fast)] = read {
                return settle([slow, normal, fast]);
            }
            tracing::debug!(target: TARGET, chain, "the mempool histogram is empty");
        }
        Err(error) => {
            tracing::debug!(target: TARGET, chain, %error, "no histogram from the server");
        }
    }

    settle([relay_floor; 3])
}

#[derive(Debug, Default)]
pub(crate) struct FeeCache {
    held: Mutex<Option<Reading>>,
}

#[derive(Debug, Clone, Copy)]
struct Reading {
    at_elapsed_millis: u64,
    context: FeeContext,
}

impl FeeCache {
    /// Holds the lock across the fetch so a burst of quotes makes one round trip.
    pub(crate) async fn get(
        &self,
        params: &UtxoParams,
        client: &UtxoClient,
        clock: &dyn Clock,
    ) -> FeeContext {
        let now_millis = clock.elapsed_millis();
        let mut held = self.held.lock().await;
        if let Some(reading) = *held
            && now_millis.saturating_sub(reading.at_elapsed_millis) < FEE_CACHE_TTL_MILLIS
        {
            return reading.context;
        }
        let fresh = match params.fee_market {
            FeeMarket::Auction(auction) => bid(params, &auction, client).await,
            FeeMarket::Flat => flat(params),
        };
        *held = Some(Reading { at_elapsed_millis: now_millis, context: fresh });
        fresh
    }
}

/// The fee at `rate`, and what a send-max would move.
///
/// Builds the real transaction, so the fee is the one that would be paid. When the
/// balance can't cover a fixed amount, it prices a one-input, two-output transaction
/// from the change address instead, so a fee still shows while the amount is too high.
fn price(
    plan: &Plan,
    fee_context: &FeeContext,
    rate: FeeRate,
) -> Result<(Amount, Option<Amount>), UtxoError> {
    match send::build(fee_context, plan, rate) {
        Ok(built) => {
            Ok((built.selection.fee, plan.is_send_max().then_some(built.selection.recipient_value)))
        }
        Err(UtxoError::InsufficientFunds { .. }) if !plan.is_send_max() => {
            let change = plan.change_script();
            let predicted =
                predict_weight([change.kind()], [plan.to_script(), change.script_pubkey()]);
            Ok((fee_for(rate, predicted)?, None))
        }
        Err(error) => Err(error),
    }
}

/// The fee at `rate` and at each of the table's levels.
pub(crate) fn quote(
    plan: &Plan,
    fee_context: &FeeContext,
    rate: FeeRate,
) -> Result<FeeQuote, UtxoError> {
    let (fee, max_amount) = price(plan, fee_context, rate)?;
    let levels = level_fees(fee_context, |level_rate| {
        price(plan, fee_context, level_rate).map(|(level_fee, _)| level_fee)
    });
    Ok(FeeQuote { fee, custom: Some(custom(rate)), max_amount, levels })
}

/// What `fee_at` charges at each of the table's levels.
///
/// A level that can't be built, such as one a send-max can't cover, is left out
/// instead of failing the whole quote.
pub(crate) fn level_fees(
    fee_context: &FeeContext,
    fee_at: impl Fn(FeeRate) -> Result<Amount, UtxoError>,
) -> Vec<LevelFee> {
    fee_context
        .params
        .fee_levels()
        .iter()
        .filter_map(|&level| {
            fee_at(fee_context.level(level)).ok().map(|fee| LevelFee { level, fee })
        })
        .collect()
}

pub(crate) fn custom(rate: FeeRate) -> FeeSelection {
    FeeSelection::Rate { rate: rate.format_sat_per_vb() }
}

/// The rate a level pays, or the user's own rate.
pub(crate) fn rate_of(
    selection: &FeeSelection,
    fee_context: &FeeContext,
) -> Result<FeeRate, UtxoError> {
    let params = &fee_context.params;
    match *selection {
        FeeSelection::Level { level } => Ok(fee_context.level(level)),
        FeeSelection::Rate { ref rate } => FeeRate::parse_sat_per_vb(rate)
            .map_err(|source| UtxoError::FeeRate { unit: params.fee_rate_unit, source }),
        FeeSelection::Evm { .. } | FeeSelection::Solana { .. } | FeeSelection::Total { .. } => {
            Err(UtxoError::ForeignFeeSelection { chain: params.chain })
        }
    }
}

/// The rate a bump at `selection` pays.
///
/// A level pays its rate or the bump's floor, whichever is higher, as Bitcoin Core
/// prices a bump. A custom rate is never raised.
pub(crate) fn bump_rate_of(
    selection: &FeeSelection,
    fee_context: &FeeContext,
    floor: FeeRate,
) -> Result<FeeRate, UtxoError> {
    match *selection {
        FeeSelection::Level { level } => Ok(fee_context.level(level).max(floor)),
        FeeSelection::Rate { .. }
        | FeeSelection::Evm { .. }
        | FeeSelection::Solana { .. }
        | FeeSelection::Total { .. } => rate_of(selection, fee_context),
    }
}
