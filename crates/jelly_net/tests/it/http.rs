use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{ChainId, ProxySetting, Transport};
use jelly_net::{Endpoint, HttpClient, HttpRequest, NetError, UrlRejection};
use parking_lot::Mutex;
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

use crate::common::read_head;

/// A remote name the endpoint policy refuses over plaintext. The fake proxy answers for
/// it, so nothing leaves the machine.
const REMOTE: &str = "plaintext-node.example";

/// Every request line the fake proxy read.
type Lines = Arc<Mutex<Vec<String>>>;

/// One listener that is both a loopback endpoint and the HTTP proxy every request goes
/// through. It redirects the endpoint to a plaintext remote host and answers the remote
/// host itself.
async fn redirecting_proxy() -> io::Result<(SocketAddr, Lines, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let lines: Lines = Arc::new(Mutex::new(Vec::new()));
    let server = tokio::spawn({
        let lines = Arc::clone(&lines);
        async move {
            let mut connections = JoinSet::new();
            while let Ok((stream, _)) = listener.accept().await {
                connections.spawn(serve(stream, Arc::clone(&lines)));
            }
        }
    });
    Ok((address, lines, server))
}

async fn serve(mut stream: TcpStream, lines: Lines) {
    loop {
        let Ok(head) = read_head(&mut stream).await else { return };
        let line = head.lines().next().unwrap_or_default().to_owned();
        let reply = if line.contains(REMOTE) {
            "HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok".to_owned()
        } else {
            format!(
                "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://{REMOTE}/rpc\r\n\
                 content-length: 0\r\n\r\n"
            )
        };
        lines.lock().push(line);
        if stream.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// A redirect must not carry a request to a host and scheme nobody configured.
///
/// The endpoint policy admits plaintext only to loopback or with the user's say-so, and
/// the pools key health and identity on the configured host.
#[tokio::test]
async fn a_redirect_to_a_plaintext_remote_host_is_not_followed() {
    let refused = Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &format!("http://{REMOTE}"))
        .expect_err("plaintext to a remote host needs permission");
    assert!(
        matches!(refused, NetError::InvalidUrl { reason: UrlRejection::PlaintextNotAllowed }),
        "{refused:?}"
    );

    let (address, lines, server) = redirecting_proxy().await.expect("a loopback proxy");
    let proxy = ProxySetting::Http { url: format!("http://{address}") };
    let http = HttpClient::new(Some(&proxy)).expect("the client builds");
    let endpoint =
        Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &format!("http://{address}"))
            .expect("a loopback endpoint");

    let sent = timeout(
        Duration::from_secs(10),
        http.send(&endpoint, "", HttpRequest::get(Duration::from_secs(5))),
    )
    .await;
    server.abort();
    let response = sent.expect("inside its budget").expect("the 307 comes back");
    assert_eq!(response.status(), 307);

    let seen = lines.lock().clone();
    assert!(
        seen.iter().all(|line| !line.contains(REMOTE)),
        "the client sent a plaintext request to a host the policy refuses: {seen:?}"
    );
}

const CHUNK_BYTES: usize = 1024;

const CHUNKS: usize = 16;

/// A loopback HTTP server that answers every request with a chunked body, so the client
/// learns the body's size only as it arrives.
async fn streaming_server() -> io::Result<(String, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            connections.spawn(stream_body(stream));
        }
    });
    Ok((url, server))
}

async fn stream_body(mut stream: TcpStream) -> io::Result<()> {
    read_head(&mut stream).await?;
    stream
        .write_all(b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n")
        .await?;
    let chunk = format!("{CHUNK_BYTES:x}\r\n{}\r\n", "a".repeat(CHUNK_BYTES));
    for _ in 0..CHUNKS {
        stream.write_all(chunk.as_bytes()).await?;
        stream.flush().await?;
    }
    stream.write_all(b"0\r\n\r\n").await
}

#[tokio::test]
async fn a_response_body_that_streams_past_the_cap_is_refused() {
    let (url, server) = streaming_server().await.expect("a loopback server");
    let endpoint = Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &url).expect("loopback");
    let http = HttpClient::new(None).expect("the client builds");
    let whole = CHUNK_BYTES.saturating_mul(CHUNKS);
    let budget = Duration::from_secs(5);

    let refused = http
        .send(&endpoint, "", HttpRequest::get(budget).with_max_bytes(whole.saturating_sub(1)))
        .await;
    let fitting = http.send(&endpoint, "", HttpRequest::get(budget).with_max_bytes(whole)).await;
    server.abort();

    assert!(matches!(refused, Err(NetError::Protocol { .. })), "{refused:?}");
    let body =
        fitting.expect("a body exactly at the cap is read").into_bytes().expect("a success status");
    assert_eq!(body.len(), whole);
}

const DECODED_BYTES: usize = 16_384;

/// `DECODED_BYTES` of `a`, gzipped.
const GZIPPED: [u8; 52] = [
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x0a, 0xed, 0xc1, 0x31, 0x01, 0x00, 0x00,
    0x00, 0xc2, 0xa0, 0xac, 0xeb, 0x5f, 0xc2, 0x18, 0x3e, 0x40, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x6f, 0x03, 0xfb, 0x44, 0xee, 0xeb,
    0x00, 0x40, 0x00, 0x00,
];

/// A loopback HTTP server that gzips its answer only when the request accepts gzip, as a
/// real server does.
async fn gzip_server() -> io::Result<(String, JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let url = format!("http://{}", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        while let Ok((stream, _)) = listener.accept().await {
            connections.spawn(gzip_body(stream));
        }
    });
    Ok((url, server))
}

async fn gzip_body(mut stream: TcpStream) -> io::Result<()> {
    let head = read_head(&mut stream).await?.to_ascii_lowercase();
    if !head.contains("accept-encoding: gzip") {
        return stream
            .write_all(
                b"HTTP/1.1 406 Not Acceptable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            )
            .await;
    }
    let reply = format!(
        "HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        GZIPPED.len()
    );
    stream.write_all(reply.as_bytes()).await?;
    stream.write_all(&GZIPPED).await
}

/// The cap counts the decoded body, so a small compressed answer cannot unpack past it.
#[tokio::test]
async fn a_gzip_body_is_decoded_and_capped_at_its_decoded_size() {
    let (url, server) = gzip_server().await.expect("a loopback server");
    let endpoint = Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &url).expect("loopback");
    let http = HttpClient::new(None).expect("the client builds");
    let budget = Duration::from_secs(5);

    let refused = http
        .send(
            &endpoint,
            "",
            HttpRequest::get(budget).with_max_bytes(DECODED_BYTES.saturating_sub(1)),
        )
        .await;
    let fitting =
        http.send(&endpoint, "", HttpRequest::get(budget).with_max_bytes(DECODED_BYTES)).await;
    server.abort();

    assert!(matches!(refused, Err(NetError::Protocol { .. })), "{refused:?}");
    let body =
        fitting.expect("a body exactly at the cap is read").into_bytes().expect("a success status");
    assert_eq!(body, vec![b'a'; DECODED_BYTES]);
}
