use jelly_core::FeeRate;
use jelly_electrum::{
    ElectrumError, EstimationMode, FeeReading, Method, ProtocolVersion,
    test_util::{Answer, FakeElectrum},
};

use crate::common::{
    ESTIMATEFEE, FEE_HISTOGRAM, MEMPOOL_GET_INFO, answer_of, number_at, old_server_version, raw,
    rows, vector,
};

#[tokio::test]
async fn every_recorded_estimate_converts_to_base_units_per_kwu() {
    for case in rows(&vector(ESTIMATEFEE), "/cases") {
        let fake = FakeElectrum::new();
        fake.always(Method::EstimateFee, answer_of(&case));
        let client = fake.client().await.expect("the fake negotiates");
        let target_blocks = u16::try_from(number_at(&case, "/target")).expect("a target");
        let reading = client
            .estimate_fee(target_blocks, EstimationMode::Conservative)
            .await
            .expect("an estimate");
        let wanted = case
            .get("sat_per_kwu")
            .and_then(serde_json::Value::as_u64)
            .map_or(FeeReading::Absent, |rate| FeeReading::Rate(FeeRate::from_sat_per_kwu(rate)));
        assert_eq!(reading, wanted, "target {target_blocks}");
    }
}

#[tokio::test]
async fn the_estimator_mode_reaches_a_new_server_and_not_an_old_one() {
    let fake = FakeElectrum::new();
    fake.always(Method::EstimateFee, Answer::Result("0.00001".to_owned()));
    let client = fake.client().await.expect("the fake negotiates");
    client.estimate_fee(6, EstimationMode::Economical).await.expect("an estimate");
    let asked = fake.calls_to(Method::EstimateFee);
    assert!(asked.first().is_some_and(|call| call.params.contains("ECONOMICAL")));

    let old = FakeElectrum::new();
    old.always(Method::ServerVersion, old_server_version());
    old.always(Method::EstimateFee, Answer::Result("0.00001".to_owned()));
    let client = old.client().await.expect("the fake negotiates");
    assert_eq!(client.protocol().await, Some(ProtocolVersion::V1_4));
    client.estimate_fee(6, EstimationMode::Economical).await.expect("an estimate");
    let asked = old.calls_to(Method::EstimateFee);
    assert!(asked.first().is_some_and(|call| !call.params.contains("ECONOMICAL")), "{asked:?}");
}

#[tokio::test]
async fn the_histogram_decodes_into_integer_rates() {
    let fake = FakeElectrum::new();
    fake.always(Method::FeeHistogram, Answer::Result(raw(FEE_HISTOGRAM, "/result")));
    let client = fake.client().await.expect("the fake negotiates");
    let buckets = client.fee_histogram().await.expect("a histogram");
    let expect = rows(&vector(FEE_HISTOGRAM), "/expect");
    assert_eq!(buckets.len(), expect.len());
    for (bucket, want) in buckets.iter().zip(&expect) {
        assert_eq!(bucket.fee_rate.sat_per_kwu(), number_at(want, "/sat_per_kwu"));
        assert_eq!(bucket.vsize, number_at(want, "/vsize"));
    }
}

#[tokio::test]
async fn the_relay_floor_comes_from_mempool_get_info() {
    let fake = FakeElectrum::new();
    fake.always(Method::MempoolInfo, Answer::Result(raw(MEMPOOL_GET_INFO, "/result")));
    let client = fake.client().await.expect("the fake negotiates");
    let info = client.mempool_info().await.expect("the mempool info");
    let want = vector(MEMPOOL_GET_INFO);
    assert_eq!(
        info.mempool_min_fee.map(FeeRate::sat_per_kwu),
        Some(number_at(&want, "/expect/mempool_min_fee_sat_per_kwu"))
    );
    assert_eq!(
        info.min_relay_tx_fee.map(FeeRate::sat_per_kwu),
        Some(number_at(&want, "/expect/min_relay_tx_fee_sat_per_kwu"))
    );
    assert_eq!(
        info.incremental_relay_fee.map(FeeRate::sat_per_kwu),
        Some(number_at(&want, "/expect/incremental_relay_fee_sat_per_kwu"))
    );
    assert_eq!(
        info.relay_floor().map(FeeRate::sat_per_kwu),
        Some(number_at(&want, "/expect/relay_floor_sat_per_kwu"))
    );
}

#[tokio::test]
async fn a_congested_mempool_raises_the_floor_above_the_relay_minimum() {
    let fake = FakeElectrum::new();
    fake.always(Method::MempoolInfo, Answer::Result(raw(MEMPOOL_GET_INFO, "/congested/result")));
    let client = fake.client().await.expect("the fake negotiates");
    let info = client.mempool_info().await.expect("the mempool info");
    let want = vector(MEMPOOL_GET_INFO);
    assert_eq!(
        info.mempool_min_fee.map(FeeRate::sat_per_kwu),
        Some(number_at(&want, "/congested/expect/mempool_min_fee_sat_per_kwu"))
    );
    assert_eq!(
        info.relay_floor().map(FeeRate::sat_per_kwu),
        Some(number_at(&want, "/congested/expect/relay_floor_sat_per_kwu"))
    );
    assert!(info.relay_floor() > info.min_relay_tx_fee);
}

/// Fulcrum copies only the keys the daemon returned, and Litecoin Core's
/// `getmempoolinfo` has no `incrementalrelayfee`, so this is a Litecoin
/// server's whole answer.
#[tokio::test]
async fn mempool_info_without_an_incremental_fee_keeps_the_floors_it_states() {
    let fake = FakeElectrum::new();
    fake.always(
        Method::MempoolInfo,
        Answer::Result(
            r#"{"mempoolminfee":0.00003471,"minrelaytxfee":0.00001,"unbroadcastcount":0}"#
                .to_owned(),
        ),
    );
    let client = fake.client().await.expect("the fake negotiates");
    let info = client.mempool_info().await.expect("the floors the server did state");
    assert_eq!(info.mempool_min_fee, Some(FeeRate::from_sat_per_kwu(868)));
    assert_eq!(info.min_relay_tx_fee, Some(FeeRate::from_sat_per_kwu(250)));
    assert_eq!(info.incremental_relay_fee, None, "an absent floor is not a rate of zero");
    assert_eq!(info.relay_floor(), Some(FeeRate::from_sat_per_kwu(868)));
}

#[tokio::test]
async fn a_server_below_protocol_one_point_six_has_no_mempool_info() {
    let fake = FakeElectrum::new();
    fake.always(Method::ServerVersion, old_server_version());
    let client = fake.client().await.expect("the fake negotiates");
    assert_eq!(client.protocol().await, Some(ProtocolVersion::V1_4));
    let error = client.mempool_info().await.expect_err("the method is gated");
    assert!(matches!(error, ElectrumError::MethodUnavailable { .. }), "{error}");
    assert!(!fake.has_been_called(Method::MempoolInfo), "the call never reached the wire");
}
