//! The fee service over a loopback host.

use std::sync::Arc;
use std::time::Duration;

use jelly_core::{Clock, FeeRate, Transport, test_util::FakeClock};
use jelly_net::{
    Endpoint, HttpClient, HttpConnector, NetRuntime, NoIdentity, PacerPolicy,
    test_util::LoopbackHttp,
};
use jelly_utxo::{Auction, FeeApi as _, MempoolFeeApi, UtxoParams};
use tokio::time::timeout;

use crate::common::{BITCOIN, Fallible, auction};

const WHOLE_NUMBER_ROUTE: &str = "/v1/fees/recommended";

const RECOMMENDED: &str =
    r#"{"fastestFee":12,"halfHourFee":8,"hourFee":4,"economyFee":2,"minimumFee":1}"#;

/// Answers like a deployment that predates the precise route: 404 for every route but
/// the whole-number one.
async fn fee_host() -> Fallible<LoopbackHttp> {
    Ok(LoopbackHttp::start(|request| {
        if request.path() == WHOLE_NUMBER_ROUTE {
            ("200 OK", RECOMMENDED.to_owned())
        } else {
            ("404 Not Found", String::new())
        }
    })
    .await?)
}

fn paths(host: &LoopbackHttp) -> Vec<String> {
    host.requests().iter().map(|request| request.path().to_owned()).collect()
}

fn fee_api(params: &UtxoParams, auction: &Auction, url: &str) -> Fallible<MempoolFeeApi> {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let runtime = NetRuntime::with_pacing(clock, PacerPolicy::UNTHROTTLED);
    let connector = HttpConnector::new(HttpClient::new(None)?, Arc::new(NoIdentity));
    let endpoint = Endpoint::parse(params.chain, Transport::FeeApi, url)?;
    let pool = runtime.pool(params.chain, Transport::FeeApi, Arc::new(connector), vec![endpoint]);
    Ok(MempoolFeeApi::new(Arc::new(pool), auction))
}

#[tokio::test]
async fn a_host_without_the_precise_route_is_read_through_the_whole_number_one() {
    let host = fee_host().await.unwrap();
    let market = auction(&BITCOIN).unwrap();
    let api = fee_api(&BITCOIN, &market, host.url()).unwrap();
    let fees = timeout(Duration::from_secs(20), api.recommended()).await.unwrap().unwrap();

    assert_eq!(fees.half_hour, FeeRate::from_sat_per_kwu(2_000));
    assert_eq!(paths(&host), [market.api_route, WHOLE_NUMBER_ROUTE]);
}

#[tokio::test]
async fn without_a_fallback_a_missing_route_fails_after_one_request() {
    let host = fee_host().await.unwrap();
    let market = Auction { api_fallback_route: None, ..auction(&BITCOIN).unwrap() };
    let api = fee_api(&BITCOIN, &market, host.url()).unwrap();
    timeout(Duration::from_secs(20), api.recommended())
        .await
        .unwrap()
        .expect_err("a host without the route has no rate to give");
    assert_eq!(paths(&host), [market.api_route]);
}
