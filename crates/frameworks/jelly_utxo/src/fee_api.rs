use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use jelly_core::FeeRate;
use jelly_electrum::{FeeReading, read_sat_per_vb};
use jelly_net::{HttpEndpoint, HttpRequest, NetError, Outcome, Pool};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;

use crate::error::UtxoError;
use crate::params::Auction;

const TARGET: &str = "jelly::utxo::fee_api";

/// Five numbers and their field names cannot exceed this, and a hostile endpoint
/// must not be able to make the process allocate.
const MAX_ANSWER_BYTES: usize = 4096;

/// A quote the fee service cannot answer falls back to the Electrum estimator, so
/// this bounds what one unresponsive request adds to a quote.
const FEE_API_TIMEOUT: Duration = Duration::from_secs(10);

/// The fee service a quote prefers over the Electrum server's own estimator.
///
/// A trait so an offline test can drive a whole quote through it without a
/// socket.
#[async_trait]
pub trait FeeApi: Send + Sync + fmt::Debug {
    async fn recommended(&self) -> Result<RecommendedFees, UtxoError>;
}

/// The mempool.space REST fee API, reached through the shared HTTP pool.
///
/// litecoinspace.org runs the same codebase, so the field names and shape match
/// on both chains and only the unit and the table's routes differ.
#[derive(Debug)]
pub struct MempoolFeeApi {
    pool: Arc<Pool<HttpEndpoint>>,
    api_route: &'static str,
    api_fallback_route: Option<&'static str>,
}

impl MempoolFeeApi {
    pub const fn new(pool: Arc<Pool<HttpEndpoint>>, auction: &Auction) -> Self {
        Self { pool, api_route: auction.api_route, api_fallback_route: auction.api_fallback_route }
    }
}

#[async_trait]
impl FeeApi for MempoolFeeApi {
    /// A deployment that predates `api_route` answers it with 404, so the same
    /// host is asked for the fallback route before the walk gives up on it.
    async fn recommended(&self) -> Result<RecommendedFees, UtxoError> {
        self.pool
            .call("fee api", classify, async |endpoint: Arc<HttpEndpoint>| {
                match (read(&endpoint, self.api_route).await, self.api_fallback_route) {
                    (Err(NetError::Status { status: 404, .. }), Some(fallback)) => {
                        tracing::debug!(
                            target: TARGET,
                            endpoint = %endpoint.label(),
                            "the fee service lacks the preferred route; the fallback is read"
                        );
                        read(&endpoint, fallback).await
                    }
                    (answered, _) => answered,
                }
            })
            .await
            .map_err(UtxoError::from_net)
    }
}

async fn read(endpoint: &HttpEndpoint, route: &str) -> Result<RecommendedFees, NetError> {
    let request = HttpRequest::get(FEE_API_TIMEOUT).with_max_bytes(MAX_ANSWER_BYTES);
    let body = endpoint.send(route, request).await?.into_bytes()?;
    // A parse failure is logged by position, because serde's message would quote
    // the service's text back.
    serde_json::from_slice::<RecommendedFees>(&body).map_err(|source| {
        tracing::debug!(
            target: TARGET,
            endpoint = %endpoint.label(),
            line = source.line(),
            column = source.column(),
            "the recommended-fee answer does not parse"
        );
        NetError::protocol(endpoint.label(), "the recommended-fee answer is not readable")
    })
}

/// A fee service that refuses the request would refuse it everywhere, and
/// anything else is worth another endpoint.
const fn classify(error: &NetError) -> Outcome {
    match *error {
        NetError::Status { status: 400 | 401 | 403 | 404, .. } => Outcome::Fail,
        NetError::Endpoint { .. }
        | NetError::Timeout { .. }
        | NetError::Protocol { .. }
        | NetError::Status { .. }
        | NetError::RateLimited { .. }
        | NetError::Rpc { .. }
        | NetError::WrongNetwork { .. }
        | NetError::NoEndpoints { .. }
        | NetError::NoneReady { .. }
        | NetError::AllFailed { .. }
        | NetError::InvalidUrl { .. }
        | NetError::Defaults { .. }
        | NetError::Config { .. } => Outcome::Rotate,
    }
}

/// `/v1/fees/recommended` or `/v1/fees/precise` as a mempool.space-family service
/// answers it.
///
/// mempool.space labels `hourFee`, `halfHourFee`, and `fastestFee` low, medium, and
/// high priority, and Sparrow and Trezor Suite offer the same three, so they are the
/// slow, normal, and fast levels. `minimumFee` is the service's view of the relay
/// floor. `economyFee` is served too but not read.
///
/// Every field is a rate in base units per virtual byte. They arrive as JSON numbers
/// and are read as decimal text, rounding up, so no float touches the fee path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecommendedFees {
    pub hour: FeeRate,
    pub half_hour: FeeRate,
    pub fastest: FeeRate,
    /// Absent on services that do not report a floor.
    pub minimum: Option<FeeRate>,
}

impl<'de> Deserialize<'de> for RecommendedFees {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            #[serde(rename = "hourFee")]
            hour: Box<RawValue>,
            #[serde(rename = "halfHourFee")]
            half_hour: Box<RawValue>,
            #[serde(rename = "fastestFee")]
            fastest: Box<RawValue>,
            #[serde(rename = "minimumFee", default)]
            minimum: Option<Box<RawValue>>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            hour: rate::<D>(&wire.hour)?,
            half_hour: rate::<D>(&wire.half_hour)?,
            fastest: rate::<D>(&wire.fastest)?,
            minimum: wire.minimum.as_deref().map(rate::<D>).transpose()?,
        })
    }
}

/// [`read_sat_per_vb`] converts in integers and rounds up, because every
/// consumer of a rate uses it as a floor.
fn rate<'de, D: Deserializer<'de>>(raw: &RawValue) -> Result<FeeRate, D::Error> {
    read_sat_per_vb(raw.get())
        .and_then(FeeReading::rate)
        .ok_or_else(|| D::Error::custom("a recommended fee is not a rate this build can read"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A busy mempool.space answer and a busy litecoinspace.org one.
    #[test]
    fn a_recommended_answer_reads_as_integer_rates() {
        let cases = [
            (
                r#"{"fastestFee":12,"halfHourFee":8,"hourFee":4,"economyFee":2,"minimumFee":1}"#,
                [1_000, 2_000, 3_000],
                250,
            ),
            (
                r#"{"fastestFee":29,"halfHourFee":25,"hourFee":21,"economyFee":10,"minimumFee":5}"#,
                [5_250, 6_250, 7_250],
                1_250,
            ),
        ];
        for (body, [hour, half_hour, fastest], minimum) in cases {
            let fees: RecommendedFees = serde_json::from_str(body).expect("the body parses");
            assert_eq!(fees.hour, FeeRate::from_sat_per_kwu(hour), "{body}");
            assert_eq!(fees.half_hour, FeeRate::from_sat_per_kwu(half_hour), "{body}");
            assert_eq!(fees.fastest, FeeRate::from_sat_per_kwu(fastest), "{body}");
            assert_eq!(fees.minimum, Some(FeeRate::from_sat_per_kwu(minimum)), "{body}");
        }
    }

    /// The precise route answers to a thousandth of a sat/vB, below one when the
    /// mempool is quiet.
    #[test]
    fn a_precise_rate_stays_exact_and_the_floor_may_be_absent() {
        let body = r#"{"fastestFee":1.5,"halfHourFee":1.004,"hourFee":1,"economyFee":0.5}"#;
        let fees: RecommendedFees = serde_json::from_str(body).expect("the body parses");
        assert_eq!(fees.hour, FeeRate::from_sat_per_kwu(250));
        assert_eq!(fees.half_hour, FeeRate::from_sat_per_kwu(251));
        assert_eq!(fees.fastest, FeeRate::from_sat_per_kwu(375));
        assert_eq!(fees.minimum, None);

        let body = r#"{"fastestFee":1.01,"halfHourFee":0.756,"hourFee":0.5,"minimumFee":0.1}"#;
        let fees: RecommendedFees = serde_json::from_str(body).expect("the body parses");
        assert_eq!(fees.hour, FeeRate::from_sat_per_kwu(125));
        assert_eq!(fees.half_hour, FeeRate::from_sat_per_kwu(189));
        assert_eq!(fees.fastest, FeeRate::from_sat_per_kwu(253));
        assert_eq!(fees.minimum, Some(FeeRate::from_sat_per_kwu(25)));
    }

    /// The litecoinspace.org answers the Litecoin file records, with the half-hour
    /// rate and the floor each is read as.
    #[test]
    fn the_recorded_litecoin_answers_read_as_their_recorded_rates() {
        let file: serde_json::Value =
            serde_json::from_str(include_str!("../tests/vectors/litecoin/fee_rates.json")).unwrap();
        let row = |pointer: &str| file.pointer(pointer).cloned().unwrap_or_default();
        let kwu = |value: &serde_json::Value, key: &str| {
            FeeRate::from_sat_per_kwu(value.get(key).and_then(serde_json::Value::as_u64).unwrap())
        };

        let recommended = row("/recommended");
        let fees: RecommendedFees =
            serde_json::from_value(row("/recommended/answer")).expect("the answer parses");
        assert_eq!(fees.half_hour, kwu(&recommended, "kwu"));
        assert_eq!(fees.minimum, Some(kwu(&recommended, "floorKwu")));

        // A quiet service prices everything at one lit/vB, the relay floor.
        let fees: RecommendedFees =
            serde_json::from_value(row("/quiet/answer")).expect("the answer parses");
        assert_eq!(fees.half_hour, kwu(&file, "relayFloorKwu"));
        assert_eq!(fees.minimum, Some(kwu(&file, "relayFloorKwu")));
    }

    #[test]
    fn a_negative_or_unreadable_rate_is_refused_rather_than_defaulted() {
        for body in [
            r#"{"fastestFee":1,"halfHourFee":-1,"hourFee":1}"#,
            r#"{"fastestFee":1,"halfHourFee":"fast","hourFee":1}"#,
            r#"{"fastestFee":1,"hourFee":1}"#,
            r#"{"fastestFee":1,"halfHourFee":1}"#,
            r#"{"halfHourFee":1,"hourFee":1}"#,
            "not json at all",
        ] {
            assert!(serde_json::from_str::<RecommendedFees>(body).is_err(), "{body}");
        }
    }

    #[test]
    fn the_only_statuses_that_stop_the_rotation_are_the_ones_every_host_would_repeat() {
        let label = jelly_core::EndpointLabel::new("https", "fees.invalid", None);
        for status in [400, 401, 403, 404] {
            let error = NetError::Status { endpoint: label.clone(), status };
            assert_eq!(classify(&error), Outcome::Fail, "{status}");
        }
        let error = NetError::Status { endpoint: label, status: 500 };
        assert_eq!(classify(&error), Outcome::Rotate);
    }
}
