//! TRON prices are set by governance rather than a market, and a transaction
//! costs what the resource model says at the prices the chain publishes.
//!
//! Those prices move, as when `getEnergyFee` fell from 210 to 100 sun in August
//! 2025, so they are read at most every five minutes and cached. The compiled
//! figures are only a stand-in, and a quote says when it uses them.

use std::time::Duration;

use jelly_core::{Clock, Deadline};
use tokio::sync::Mutex as AsyncMutex;

use crate::client::TronClient;
use crate::shape::ChainParameters;

const TARGET: &str = "jelly::tron::params";

/// How long the governance parameters are reused before being read again.
const PARAMS_TTL: Duration = Duration::from_secs(300);

/// Governance parameters. Prices and fees are in sun.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChainParams {
    /// `getTransactionFee`, in sun per bandwidth point.
    pub(crate) bandwidth_price_sun: u64,
    /// `getEnergyFee`, in sun per energy unit.
    pub(crate) energy_price_sun: u64,
    /// `getCreateAccountFee`, the flat bandwidth charge when a transfer
    /// activates an account without enough staked bandwidth.
    pub(crate) create_account_fee_sun: u64,
    /// `getCreateNewAccountFeeInSystemContract`, charged on every activation,
    /// staked bandwidth or not.
    pub(crate) create_account_contract_fee_sun: u64,
    /// `getCreateNewAccountBandwidthRate`, a multiplier on the bandwidth points
    /// of an activation.
    pub(crate) create_account_bandwidth_rate: u64,
    /// `getMaxFeeLimit`, the protocol ceiling on `raw_data.fee_limit`.
    pub(crate) max_fee_limit_sun: u64,
    /// False when a node did not report a usable value for both prices a quote
    /// is built on and the compiled figures are standing in.
    pub(crate) is_from_chain: bool,
}

impl ChainParams {
    /// Mainnet's values on 2026-08-14. A quote built from these carries a
    /// warning, because a price that has since risen would understate the cost.
    pub(crate) const FALLBACK: Self = Self {
        bandwidth_price_sun: 1_000,
        energy_price_sun: 100,
        create_account_fee_sun: 100_000,
        create_account_contract_fee_sun: 1_000_000,
        create_account_bandwidth_rate: 1,
        max_fee_limit_sun: 15_000_000_000,
        is_from_chain: false,
    };
}

impl From<&ChainParameters> for ChainParams {
    /// Keeps the compiled value for any key the node omits or reports as
    /// negative.
    ///
    /// `is_from_chain` holds only when both prices a quote is built on were read
    /// from the node, so a partial or unusable answer is still labelled as
    /// compiled.
    fn from(reported: &ChainParameters) -> Self {
        let reported_value = |key: &str| {
            let value = reported.get(key)?;
            u64::try_from(value).ok()
        };
        let read = |key: &str, fallback: u64| reported_value(key).unwrap_or(fallback);
        let base = Self::FALLBACK;
        Self {
            bandwidth_price_sun: read("getTransactionFee", base.bandwidth_price_sun),
            energy_price_sun: read("getEnergyFee", base.energy_price_sun),
            create_account_fee_sun: read("getCreateAccountFee", base.create_account_fee_sun),
            create_account_contract_fee_sun: read(
                "getCreateNewAccountFeeInSystemContract",
                base.create_account_contract_fee_sun,
            ),
            create_account_bandwidth_rate: read(
                "getCreateNewAccountBandwidthRate",
                base.create_account_bandwidth_rate,
            ),
            max_fee_limit_sun: read("getMaxFeeLimit", base.max_fee_limit_sun),
            is_from_chain: reported_value("getTransactionFee").is_some()
                && reported_value("getEnergyFee").is_some(),
        }
    }
}

/// The cached copy of the governance parameters.
///
/// The mutex is held across the fetch, so a burst of quotes costs one read. A
/// failed read caches the compiled figures for the same interval rather than
/// re-asking on every quote, and every quote built meanwhile says which figures
/// it used.
#[derive(Debug, Default)]
pub(crate) struct ParamCache {
    held: AsyncMutex<Option<Held>>,
}

#[derive(Debug, Clone, Copy)]
struct Held {
    params: ChainParams,
    expires: Deadline,
}

impl ParamCache {
    pub(crate) async fn get(&self, client: &TronClient, clock: &dyn Clock) -> ChainParams {
        let mut held = self.held.lock().await;
        if let Some(current) = *held
            && !current.expires.is_expired(clock)
        {
            return current.params;
        }
        let params = match client.chain_parameters().await {
            Ok(params) => params,
            Err(error) => {
                tracing::debug!(
                    target: TARGET,
                    %error,
                    "no node reported the fee parameters; the next five minutes of quotes use the \
                     compiled figures and say so"
                );
                ChainParams::FALLBACK
            }
        };
        *held = Some(Held { params, expires: clock.deadline_in(PARAMS_TTL) });
        params
    }
}

#[cfg(test)]
mod tests {
    use std::future::poll_fn;
    use std::pin::pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::Poll;

    use async_trait::async_trait;
    use jelly_core::test_util::FakeClock;
    use jelly_net::NetError;

    use super::*;
    use crate::error::TronError;
    use crate::fixtures::answered;
    use crate::node::{Answered, TronNode};
    use crate::route::Request;
    use crate::test_util::endpoint;

    #[derive(Debug)]
    struct TimedNode {
        clock: Arc<FakeClock>,
        calls: AtomicUsize,
        should_fail: bool,
    }

    #[async_trait]
    impl TronNode for TimedNode {
        async fn call(&self, request: &Request) -> Result<Answered, TronError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.clock.advance(Duration::from_secs(60));
            if self.should_fail {
                return Err(TronError::Net(NetError::timed_out(
                    &endpoint(),
                    Duration::from_secs(60),
                )));
            }
            Ok(answered(
                request.route(),
                r#"{"chainParameter":[{"key":"getTransactionFee","value":1000},{"key":"getEnergyFee","value":210}]}"#,
            ))
        }

        async fn broadcast(
            &self,
            preferred: &Request,
            _fallback: &Request,
        ) -> Result<Answered, TronError> {
            self.call(preferred).await
        }
    }

    #[tokio::test]
    async fn fresh_and_fallback_parameters_get_a_full_reuse_window() {
        for should_fail in [false, true] {
            let clock = Arc::new(FakeClock::default());
            let node = Arc::new(TimedNode {
                clock: Arc::clone(&clock),
                calls: AtomicUsize::new(0),
                should_fail,
            });
            let client = TronClient::new(Arc::<TimedNode>::clone(&node));
            let cache = ParamCache::default();
            let first = cache.get(&client, clock.as_ref()).await;
            assert_eq!(first.is_from_chain, !should_fail);
            clock.advance(Duration::from_secs(299));
            assert_eq!(cache.get(&client, clock.as_ref()).await, first);
            assert_eq!(node.calls.load(Ordering::SeqCst), 1);
            clock.advance(Duration::from_secs(1));
            cache.get(&client, clock.as_ref()).await;
            assert_eq!(node.calls.load(Ordering::SeqCst), 2);
        }
    }

    #[tokio::test]
    async fn a_waiting_reader_checks_expiry_after_acquiring_the_lock() {
        let clock = Arc::new(FakeClock::default());
        let node = Arc::new(TimedNode {
            clock: Arc::clone(&clock),
            calls: AtomicUsize::new(0),
            should_fail: false,
        });
        let client = TronClient::new(Arc::<TimedNode>::clone(&node));
        let cache = ParamCache::default();
        cache.get(&client, clock.as_ref()).await;
        let held = cache.held.lock().await;
        let mut waiting = pin!(cache.get(&client, clock.as_ref()));
        assert!(poll_fn(|context| Poll::Ready(waiting.as_mut().poll(context).is_pending())).await);
        clock.advance(PARAMS_TTL);
        drop(held);
        waiting.await;
        assert_eq!(node.calls.load(Ordering::SeqCst), 2);
    }

    fn reported(body: &str) -> ChainParameters {
        serde_json::from_str(body).expect("a parameter list decodes")
    }

    #[test]
    fn a_full_answer_is_read_from_the_chain() {
        let params = ChainParams::from(&reported(
            r#"{"chainParameter":[
                {"key":"getTransactionFee","value":1000},
                {"key":"getEnergyFee","value":210},
                {"key":"getCreateAccountFee","value":100000},
                {"key":"getCreateNewAccountFeeInSystemContract","value":1000000},
                {"key":"getCreateNewAccountBandwidthRate","value":1},
                {"key":"getMaxFeeLimit","value":15000000000}]}"#,
        ));
        assert!(params.is_from_chain);
        assert_eq!(params.energy_price_sun, 210, "a moved price is read, never assumed");
    }

    #[test]
    fn a_key_a_node_omits_keeps_the_compiled_value_and_the_quote_says_compiled() {
        let params = ChainParams::from(&reported(
            r#"{"chainParameter":[{"key":"getEnergyFee","value":210}]}"#,
        ));
        assert_eq!(params.energy_price_sun, 210);
        assert_eq!(params.bandwidth_price_sun, ChainParams::FALLBACK.bandwidth_price_sun);
        assert!(!params.is_from_chain, "a partial answer is not a priced answer");
    }

    #[test]
    fn a_parameter_with_no_value_at_all_is_absent_rather_than_zero() {
        let params = ChainParams::from(&reported(
            r#"{"chainParameter":[{"key":"getTransactionFee"},{"key":"getEnergyFee","value":100}]}"#,
        ));
        assert_eq!(params.bandwidth_price_sun, ChainParams::FALLBACK.bandwidth_price_sun);
        assert!(!params.is_from_chain);
    }

    #[test]
    fn a_negative_governance_value_is_never_read_as_a_price() {
        let params = ChainParams::from(&reported(
            r#"{"chainParameter":[{"key":"getTransactionFee","value":-1},{"key":"getEnergyFee","value":100}]}"#,
        ));
        assert_eq!(params.bandwidth_price_sun, ChainParams::FALLBACK.bandwidth_price_sun);
        assert!(!params.is_from_chain, "a price the node could not state is the compiled one");
    }

    #[test]
    fn the_compiled_figures_are_the_mainnet_values_this_build_was_cut_against() {
        let compiled = ChainParams::FALLBACK;
        assert_eq!(compiled.bandwidth_price_sun, 1_000);
        assert_eq!(compiled.energy_price_sun, 100);
        assert_eq!(compiled.create_account_fee_sun, 100_000);
        assert_eq!(compiled.create_account_contract_fee_sun, 1_000_000);
        assert_eq!(compiled.create_account_bandwidth_rate, 1);
        assert_eq!(compiled.max_fee_limit_sun, 15_000_000_000);
        assert!(
            !compiled.is_from_chain,
            "the compiled figures never claim to have come from a node"
        );
    }
}
