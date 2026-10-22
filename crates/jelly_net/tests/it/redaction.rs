use std::io;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{ChainError, ChainId, Clock, FailureKind, Transport, test_util::FakeClock};
use jelly_net::{
    Endpoint, EndpointOrigin, HealthClass, HealthTracker, HttpRequest, NetError, ProbeReport,
};

use crate::common::{harness, rotate};

const SECRET: &str = "SECRET";
const KEYED: &str = "https://rpc.example/v1?api_key=SECRET";
const AUTHED: &str = "https://user:SECRET@rpc.example/v1";

fn keyed(url: &str) -> Result<Endpoint, NetError> {
    Endpoint::parse(ChainId::Ethereum, Transport::HttpJsonrpc, url)
        .map(|endpoint| endpoint.with_origin(EndpointOrigin::User))
}

#[test]
fn a_label_keeps_the_host_and_drops_everything_else() {
    for url in [KEYED, AUTHED] {
        let endpoint = keyed(url).expect("a canonical url");
        assert!(endpoint.url().contains(SECRET), "the canonical url is the identity");
        assert_eq!(endpoint.label().to_string(), "https://rpc.example");
        assert!(!endpoint.label().to_string().contains(SECRET));
        assert!(!endpoint.to_string().contains(SECRET));
        assert!(!format!("{:?}", endpoint.label()).contains(SECRET));
        assert!(!format!("{endpoint:?}").contains(SECRET), "{endpoint:?}");
    }
}

/// TRON sends its API key as a header, so a request printed in a log must not carry it.
#[test]
fn a_request_names_its_headers_and_never_their_values() {
    let headers = [("TRON-PRO-API-KEY", SECRET)];
    let body = format!(r#"{{"api_key":"{SECRET}"}}"#);
    let request = HttpRequest::post_json(&body, Duration::from_secs(5)).with_headers(&headers);

    let rendered = format!("{request:?}");
    assert!(rendered.contains("TRON-PRO-API-KEY"), "{rendered}");
    assert!(!rendered.contains(SECRET), "{rendered}");
}

#[test]
fn a_recorded_failure_carries_no_key() {
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let tracker = HealthTracker::new(clock);
    let endpoint = keyed(KEYED).expect("a canonical url");

    for _ in 0..3u8 {
        tracker.record_failure(
            &endpoint,
            &NetError::endpoint(
                endpoint.label(),
                FailureKind::Connect,
                io::Error::other("refused"),
            ),
        );
    }

    let health = tracker.entry(ChainId::Ethereum, endpoint.url()).health;
    assert!(!health.last_error.unwrap_or_default().contains(SECRET));
    assert_eq!(tracker.entry(ChainId::Ethereum, endpoint.url()).class, HealthClass::Quarantined);
}

#[test]
fn every_failure_shape_renders_without_the_key() {
    let endpoint = keyed(KEYED).expect("a canonical url");
    let label = endpoint.label().clone();
    let failures = [
        NetError::endpoint(&label, FailureKind::Tls, io::Error::other("handshake failed")),
        NetError::timed_out(&label, Duration::from_secs(15)),
        NetError::protocol(&label, "malformed"),
        NetError::Status { endpoint: label.clone(), status: 502 },
        NetError::RateLimited { endpoint: label.clone(), retry_after: None },
        NetError::Rpc { endpoint: label.clone(), code: -32000, message: "busy".to_owned() },
        NetError::WrongNetwork {
            endpoint: label.clone(),
            expected: "1".to_owned(),
            found: "56".to_owned(),
        },
        NetError::NoEndpoints { chain: ChainId::Ethereum },
        NetError::NoneReady { chain: ChainId::Ethereum, operation: "head" },
        NetError::AllFailed { chain: ChainId::Ethereum, operation: "head", attempts: Vec::new() },
    ];
    for failure in failures {
        let rendered = format!("{failure}{failure:?}");
        assert!(!rendered.contains(SECRET), "{rendered}");
        assert!(!rendered.contains("api_key"), "{rendered}");
        let report = format!("{:?}", ProbeReport::from_failure(&label, &failure));
        assert!(!report.contains(SECRET), "{report}");
        assert!(!ChainError::from(failure).to_string().contains(SECRET));
    }
}

#[tokio::test(start_paused = true)]
async fn the_all_endpoints_failed_path_carries_no_key() {
    let harness = harness(&[]).expect("an empty pool");
    let with_key = keyed(KEYED).expect("a canonical url");
    let with_auth = keyed(AUTHED).expect("a canonical url");
    harness
        .connector
        .refused_urls
        .lock()
        .extend([with_key.url().to_owned(), with_auth.url().to_owned()]);
    let pool = harness.pool_over(vec![with_key, with_auth]);

    let error = pool
        .call("head", rotate, async |connection| Ok(connection.url.clone()))
        .await
        .expect_err("both endpoints refuse");

    assert!(!error.to_string().contains(SECRET));
    for row in harness.health.snapshot(ChainId::Ethereum) {
        assert!(row.url.contains(SECRET), "the canonical url is the health key");
        assert!(!row.health.last_error.unwrap_or_default().contains(SECRET));
    }

    let chain_error = ChainError::from(error);
    let rendered = format!("{chain_error}{chain_error:?}");
    assert!(!rendered.contains(SECRET), "{rendered}");
    let ChainError::EndpointsDown { attempts, .. } = &chain_error else {
        panic!("expected endpoints down, got {chain_error}");
    };
    assert_eq!(attempts.len(), 2);
    for attempt in attempts {
        assert_eq!(attempt.endpoint.host(), "rpc.example");
        assert!(!attempt.message.contains(SECRET));
    }
}
