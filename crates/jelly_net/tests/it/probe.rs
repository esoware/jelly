use std::io;
use std::time::Duration;

use jelly_core::{ChainId, EndpointLabel, FailureKind, Transport, test_util::FakeClock};
use jelly_net::{
    Endpoint, HttpClient, NetError, ProbeFailure, ProbeReport, UrlRejection, probe, probe_reachable,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use crate::common::{ENDPOINT_A, endpoint, harness, read_head};

async fn answer_once(
    status: &'static str,
    body: String,
) -> io::Result<(String, JoinHandle<io::Result<String>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let head = read_head(&mut stream).await?;
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).await?;
        Ok(head)
    });
    Ok((url, server))
}

#[tokio::test(start_paused = true)]
async fn a_confirmed_endpoint_is_verified_with_the_time_the_dial_and_the_check_took() {
    let harness = harness(&[]).expect("a connector");
    *harness.connector.advance_on_connect.lock() = Duration::from_millis(40);

    let report = probe(
        harness.connector.as_ref(),
        harness.connector.clock.as_ref(),
        &endpoint(ENDPOINT_A).expect("an endpoint"),
    )
    .await;

    assert_eq!(report, ProbeReport::Verified { latency_ms: 40 });
    assert_eq!(*harness.connector.identity_checks.lock(), vec![ENDPOINT_A.to_owned()]);
}

#[tokio::test(start_paused = true)]
async fn a_host_on_another_network_is_reported_as_the_wrong_network() {
    let harness = harness(&[]).expect("a connector");
    harness.connector.wrong_network_urls.lock().push(ENDPOINT_A.to_owned());

    let report = probe(
        harness.connector.as_ref(),
        harness.connector.clock.as_ref(),
        &endpoint(ENDPOINT_A).expect("an endpoint"),
    )
    .await;

    assert_eq!(report, ProbeReport::WrongNetwork);
}

#[tokio::test(start_paused = true)]
async fn a_failed_dial_is_unreachable_and_asks_nothing() {
    let harness = harness(&[]).expect("a connector");
    harness.connector.refused_urls.lock().push(ENDPOINT_A.to_owned());

    let report = probe(
        harness.connector.as_ref(),
        harness.connector.clock.as_ref(),
        &endpoint(ENDPOINT_A).expect("an endpoint"),
    )
    .await;

    assert_eq!(report, ProbeReport::Unreachable { failure: ProbeFailure::NoConnection });
    assert_eq!(*harness.connector.identity_checks.lock(), Vec::<String>::new());
}

#[tokio::test(start_paused = true)]
async fn a_check_that_never_answers_runs_out_of_the_probe_budget() {
    let harness = harness(&[]).expect("a connector");
    harness.connector.silent_identity_urls.lock().push(ENDPOINT_A.to_owned());

    let report = probe(
        harness.connector.as_ref(),
        harness.connector.clock.as_ref(),
        &endpoint(ENDPOINT_A).expect("an endpoint"),
    )
    .await;

    assert_eq!(report, ProbeReport::Unreachable { failure: ProbeFailure::TimedOut });
}

#[tokio::test]
async fn a_rest_service_that_answers_at_all_is_reachable() {
    let (url, server) =
        answer_once("404 Not Found", String::new()).await.expect("a loopback server");
    let endpoint = Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &url).expect("loopback");
    let http = HttpClient::new(None).expect("the client builds");

    let report = probe_reachable(&http, &FakeClock::default(), &endpoint).await;

    assert_eq!(report, ProbeReport::Reachable { latency_ms: 0 });
    let head = server.await.expect("the server ran").expect("the request was read");
    assert!(head.starts_with("GET / HTTP/1.1\r\n"), "{head}");
}

#[tokio::test]
async fn a_rest_service_with_an_oversized_body_is_refused() {
    let (url, server) = answer_once("200 OK", "a".repeat(65_537)).await.expect("a loopback server");
    let endpoint = Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &url).expect("loopback");
    let http = HttpClient::new(None).expect("the client builds");

    let report = probe_reachable(&http, &FakeClock::default(), &endpoint).await;
    let _ = server.await.expect("the server ran");

    assert_eq!(report, ProbeReport::Unreachable { failure: ProbeFailure::BadAnswer });
}

#[tokio::test]
async fn a_rest_service_failing_on_its_own_side_is_unreachable() {
    let (url, server) =
        answer_once("503 Service Unavailable", String::new()).await.expect("a loopback server");
    let endpoint = Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &url).expect("loopback");
    let http = HttpClient::new(None).expect("the client builds");

    let report = probe_reachable(&http, &FakeClock::default(), &endpoint).await;

    assert_eq!(report, ProbeReport::Unreachable { failure: ProbeFailure::HttpStatus });
    server.await.expect("the server ran").expect("the request was read");
}

/// A loopback server that reads one TLS record, the client hello, and answers it with a
/// fatal `handshake_failure` alert, RFC 8446 section 6.2.
async fn refusing_tls() -> io::Result<(String, JoinHandle<io::Result<()>>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("https://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut header = [0u8; 5];
        stream.read_exact(&mut header).await?;
        let [_, _, _, high, low] = header;
        let mut hello = vec![0u8; usize::from(u16::from_be_bytes([high, low]))];
        stream.read_exact(&mut hello).await?;
        // An alert record, TLS 1.2 on the wire, two bytes, fatal, handshake_failure.
        stream.write_all(&[0x15, 0x03, 0x03, 0x00, 0x02, 0x02, 0x28]).await
    });
    Ok((url, server))
}

/// An HTTPS endpoint whose TLS handshake fails is a TLS failure.
///
/// `ProbeFailure::Tls` is the report for a handshake that failed, and the Electrum and
/// websocket dialler already classify one as `FailureKind::Tls`, so it must not be
/// reported as an endpoint that could not be reached.
#[tokio::test]
async fn a_failed_tls_handshake_on_the_http_path_is_reported_as_a_tls_failure() {
    let (url, server) = refusing_tls().await.expect("a loopback server");
    let endpoint = Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &url).expect("loopback");
    let http = HttpClient::new(None).expect("the client builds");

    let report = probe_reachable(&http, &FakeClock::default(), &endpoint).await;
    server.await.expect("the server ran").expect("the client hello was read");

    assert_eq!(
        report,
        ProbeReport::Unreachable { failure: ProbeFailure::Tls },
        "the server refused the tls handshake, so the probe should report a tls failure"
    );
}

#[test]
fn a_failure_is_reported_by_its_cause_and_never_by_its_text() {
    let label = EndpointLabel::new("https", "a.example", None);
    let cases = [
        (
            NetError::endpoint(&label, FailureKind::Tls, io::Error::other("bad certificate")),
            ProbeFailure::Tls,
        ),
        (
            NetError::endpoint(&label, FailureKind::Transport, io::Error::other("reset")),
            ProbeFailure::Dropped,
        ),
        (NetError::Status { endpoint: label.clone(), status: 404 }, ProbeFailure::HttpStatus),
        (
            NetError::RateLimited { endpoint: label.clone(), retry_after: None },
            ProbeFailure::RateLimited,
        ),
        (
            NetError::Rpc {
                endpoint: label.clone(),
                code: -32601,
                message: "no such method".to_owned(),
            },
            ProbeFailure::BadAnswer,
        ),
        (NetError::InvalidUrl { reason: UrlRejection::UnusedTransport }, ProbeFailure::NotProbed),
    ];
    for (error, failure) in cases {
        assert_eq!(
            ProbeReport::from_failure(&label, &error),
            ProbeReport::Unreachable { failure },
            "{error}"
        );
    }
}
