use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use jelly_core::{
    ChainId, Clock, FailureKind, PinStore, ProxySetting, Transport,
    test_util::{FakeClock, MemoryPinStore},
};
use jelly_net::{Dialler, Endpoint, HttpClient, HttpRequest, NetError};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::common::read_head;

const TARGET_HOST: &str = "node.example";
const TARGET_PORT: u16 = 50002;

fn target() -> Result<Endpoint, NetError> {
    Endpoint::parse(
        ChainId::Bitcoin,
        Transport::ElectrumSsl,
        &format!("ssl://{TARGET_HOST}:{TARGET_PORT}"),
    )
}

fn dialler(proxy: ProxySetting) -> Result<Dialler, NetError> {
    let pins: Arc<dyn PinStore> = Arc::new(MemoryPinStore::new());
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    Dialler::pinned(pins, clock, ChainId::Bitcoin, Some(proxy), Duration::from_secs(5))
}

/// Everything one fake proxy read from the dialler.
type Seen = Arc<Mutex<Vec<u8>>>;

async fn listener() -> io::Result<(SocketAddr, TcpListener)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    Ok((address, listener))
}

async fn take(stream: &mut TcpStream, seen: &Seen, count: usize) -> io::Result<Vec<u8>> {
    let mut buffer = vec![0u8; count];
    stream.read_exact(&mut buffer).await?;
    seen.lock().extend_from_slice(&buffer);
    Ok(buffer)
}

/// A SOCKS5 proxy that accepts an unauthenticated CONNECT and records it.
fn socks_proxy(listener: TcpListener, seen: Seen) -> JoinHandle<io::Result<()>> {
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        take(&mut stream, &seen, 3).await?;
        stream.write_all(&[0x05, 0x00]).await?;

        let head = take(&mut stream, &seen, 5).await?;
        let name_length = usize::from(head.get(4).copied().unwrap_or_default());
        take(&mut stream, &seen, name_length.saturating_add(2)).await?;
        stream.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]).await
    })
}

/// An HTTP proxy that accepts a CONNECT tunnel and records the request head.
fn http_proxy(
    listener: TcpListener,
    seen: Seen,
    status: &'static str,
) -> JoinHandle<io::Result<()>> {
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let head = read_head(&mut stream).await?;
        seen.lock().extend_from_slice(head.as_bytes());
        stream.write_all(status.as_bytes()).await
    })
}

#[tokio::test]
async fn a_socks_proxy_is_handed_the_target_as_a_name() {
    let (address, listening) = listener().await.expect("a loopback port");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let proxy_task = socks_proxy(listening, Arc::clone(&seen));

    let proxy = ProxySetting::Socks5h { url: format!("socks5h://{address}") };
    let error = dialler(proxy)
        .expect("a pinned dialler")
        .connect(&target().expect("an electrum endpoint"))
        .await
        .expect_err("the fake proxy drops the socket before any tls");

    let bytes = seen.lock().clone();
    assert_eq!(bytes.get(..3), Some([0x05, 0x01, 0x00].as_slice()), "an unauthenticated greeting");
    // 0x01 CONNECT, 0x03 domain name, then the name and the port big-endian.
    assert_eq!(bytes.get(3..8), Some([0x05, 0x01, 0x00, 0x03, 12].as_slice()));
    assert_eq!(bytes.get(8..20), Some(TARGET_HOST.as_bytes()));
    assert_eq!(bytes.get(20..22), Some(TARGET_PORT.to_be_bytes().as_slice()));
    // The tunnel opened, and what failed is the handshake inside it.
    assert!(matches!(error, NetError::Endpoint { .. }), "{error}");
    proxy_task.await.expect("the proxy task ran").expect("the proxy read the whole request");
}

#[tokio::test]
async fn a_socks_proxy_address_with_credentials_authenticates_with_them() {
    let (address, listening) = listener().await.expect("a loopback port");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let proxy_task = tokio::spawn(async move {
        let (mut stream, _) = listening.accept().await?;
        take(&mut stream, &recorder, 3).await?;
        stream.write_all(&[0x05, 0x02]).await?;
        take(&mut stream, &recorder, 13).await?;
        stream.write_all(&[0x01, 0x00]).await
    });

    let proxy = ProxySetting::Socks5h { url: format!("socks5h://tor:circuit@{address}") };
    let _ = dialler(proxy)
        .expect("a pinned dialler")
        .connect(&target().expect("an electrum endpoint"))
        .await
        .expect_err("the fake proxy drops the socket after authenticating");

    let bytes = seen.lock().clone();
    assert_eq!(bytes.get(..3), Some([0x05, 0x01, 0x02].as_slice()), "a password greeting");
    assert_eq!(bytes.get(3..5), Some([0x01, 3].as_slice()));
    assert_eq!(bytes.get(5..8), Some(b"tor".as_slice()));
    assert_eq!(bytes.get(8..9), Some([7].as_slice()));
    assert_eq!(bytes.get(9..16), Some(b"circuit".as_slice()));
    proxy_task.await.expect("the proxy task ran").expect("the proxy read the whole request");
}

#[tokio::test]
async fn an_http_proxy_is_asked_to_tunnel_to_the_target_by_name() {
    let (address, listening) = listener().await.expect("a loopback port");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let proxy_task =
        http_proxy(listening, Arc::clone(&seen), "HTTP/1.1 200 Connection established\r\n\r\n");

    let proxy = ProxySetting::Http { url: format!("http://{address}") };
    let _ = dialler(proxy)
        .expect("a pinned dialler")
        .connect(&target().expect("an electrum endpoint"))
        .await
        .expect_err("the fake proxy drops the socket before any tls");

    let head = String::from_utf8(seen.lock().clone()).expect("an ascii request head");
    assert!(head.starts_with("CONNECT node.example:50002 HTTP/1.1\r\n"), "{head}");
    assert!(head.contains("Host: node.example:50002\r\n"), "{head}");
    assert!(head.ends_with("\r\n\r\n"), "{head}");
    proxy_task.await.expect("the proxy task ran").expect("the proxy read the whole request");
}

#[tokio::test]
async fn a_silent_proxy_runs_out_of_the_connect_budget() {
    let (address, listening) = listener().await.expect("a loopback port");
    // Accept and never answer.
    let proxy_task = tokio::spawn(async move {
        let held = listening.accept().await;
        sleep(Duration::from_secs(30)).await;
        drop(held);
    });

    let pins: Arc<dyn PinStore> = Arc::new(MemoryPinStore::new());
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let proxy = ProxySetting::Socks5h { url: format!("socks5h://{address}") };
    let dialler =
        Dialler::pinned(pins, clock, ChainId::Bitcoin, Some(proxy), Duration::from_millis(150))
            .expect("a pinned dialler");

    let error = dialler
        .connect(&target().expect("an electrum endpoint"))
        .await
        .expect_err("the proxy never answered the greeting");

    assert!(matches!(error, NetError::Timeout { .. }), "{error}");
    proxy_task.abort();
}

#[tokio::test]
async fn a_refused_tunnel_fails_rather_than_reaching_the_target_directly() {
    let (address, listening) = listener().await.expect("a loopback port");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let proxy_task = http_proxy(listening, Arc::clone(&seen), "HTTP/1.1 403 Forbidden\r\n\r\n");

    let proxy = ProxySetting::Http { url: format!("http://{address}") };
    let error = dialler(proxy)
        .expect("a pinned dialler")
        .connect(&target().expect("an electrum endpoint"))
        .await
        .expect_err("the proxy refused the tunnel");

    assert!(matches!(error, NetError::Protocol { .. }), "{error}");
    proxy_task.await.expect("the proxy task ran").expect("the proxy read the whole request");
}

/// A SOCKS5 proxy that reads one CONNECT request of any address type, hands back the
/// address type and the address bytes it named, and drops the socket unanswered.
fn socks_target_recorder(listener: TcpListener) -> JoinHandle<io::Result<(u8, Vec<u8>)>> {
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let mut greeting = [0u8; 2];
        stream.read_exact(&mut greeting).await?;
        let [_, method_count] = greeting;
        let mut methods = vec![0u8; usize::from(method_count)];
        stream.read_exact(&mut methods).await?;
        stream.write_all(&[0x05, 0x00]).await?;

        let mut head = [0u8; 4];
        stream.read_exact(&mut head).await?;
        let [_, _, _, address_type] = head;
        let width = match address_type {
            0x01 => 4,
            0x04 => 16,
            _ => {
                let mut length = [0u8; 1];
                stream.read_exact(&mut length).await?;
                let [name_length] = length;
                usize::from(name_length)
            }
        };
        let mut address = vec![0u8; width];
        stream.read_exact(&mut address).await?;
        Ok((address_type, address))
    })
}

/// `ProxySetting::Socks5h` promises that the proxy resolves every name.
///
/// A user who types the common `socks5://` spelling into that route must not have the
/// HTTP client resolve hostnames locally, which the raw-socket dialler already refuses to
/// do.
#[tokio::test]
async fn a_socks5h_route_spelled_socks5_leaves_name_resolution_to_the_proxy() {
    let (address, listening) = listener().await.expect("a loopback port");
    let recorder = socks_target_recorder(listening);

    let proxy = ProxySetting::Socks5h { url: format!("socks5://{address}") };
    let http = HttpClient::new(Some(&proxy)).expect("the client builds");
    let endpoint =
        Endpoint::parse(ChainId::Ethereum, Transport::HttpJsonrpc, "https://localhost:8545")
            .expect("a loopback endpoint");
    let sent = timeout(
        Duration::from_secs(10),
        http.send(&endpoint, "", HttpRequest::get(Duration::from_secs(5))),
    )
    .await;
    let refused = sent
        .expect("the request finished inside its budget")
        .expect_err("the proxy drops the tunnel unanswered");
    assert!(
        matches!(refused, NetError::Endpoint { kind: FailureKind::Connect, .. }),
        "{refused:?}"
    );

    let (address_type, target) = timeout(Duration::from_secs(5), recorder)
        .await
        .expect("the proxy saw a connect request")
        .expect("the proxy task ran")
        .expect("the proxy read the whole request");
    assert_eq!(
        (address_type, target.as_slice()),
        (0x03, b"localhost".as_slice()),
        "the target reached the proxy as an address resolved on this machine"
    );
}

/// RFC 9110 authority-form puts an IPv6 literal in brackets, so a proxy can tell the
/// address from the port.
#[tokio::test]
async fn an_http_proxy_is_asked_for_an_ipv6_target_in_brackets() {
    let (address, listening) = listener().await.expect("a loopback port");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let proxy_task =
        http_proxy(listening, Arc::clone(&seen), "HTTP/1.1 200 Connection established\r\n\r\n");

    let target =
        Endpoint::parse(ChainId::Bitcoin, Transport::ElectrumSsl, "ssl://[2001:db8::1]:50002")
            .expect("an ipv6 electrum endpoint");
    let proxy = ProxySetting::Http { url: format!("http://{address}") };
    let _ = dialler(proxy)
        .expect("a pinned dialler")
        .connect(&target)
        .await
        .expect_err("the fake proxy drops the socket before any tls");

    let head = String::from_utf8(seen.lock().clone()).expect("an ascii request head");
    assert!(head.starts_with("CONNECT [2001:db8::1]:50002 HTTP/1.1\r\n"), "{head}");
    assert!(head.contains("Host: [2001:db8::1]:50002\r\n"), "{head}");
    proxy_task.await.expect("the proxy task ran").expect("the proxy read the whole request");
}

/// A SOCKS5 address typed without a scheme reaches the proxy from a raw socket too.
///
/// The settings page suggests `127.0.0.1:9050` for a local Tor proxy, and the shared HTTP
/// client reads that address as a SOCKS5 proxy. The raw sockets behind Electrum and
/// websockets follow the same setting, so they must reach the same proxy.
#[tokio::test]
async fn a_socks_proxy_address_without_a_scheme_reaches_the_proxy_from_a_raw_socket() {
    let (address, listening) = listener().await.expect("a loopback port");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let proxy_task = socks_proxy(listening, Arc::clone(&seen));

    let proxy = ProxySetting::Socks5h { url: address.to_string() };
    HttpClient::new(Some(&proxy)).expect("the shared http client takes the address");
    let error = dialler(proxy)
        .expect("a pinned dialler")
        .connect(&target().expect("an electrum endpoint"))
        .await
        .expect_err("the fake proxy drops the socket before any tls");
    proxy_task.abort();

    let bytes = seen.lock().clone();
    assert_eq!(
        bytes.get(..3),
        Some([0x05, 0x01, 0x00].as_slice()),
        "the raw socket should greet the socks proxy the http client uses, but it never \
         reached it and failed with: {error}"
    );
}

/// An `https://` proxy never sees its credentials in the clear.
///
/// That address asks for TLS to the proxy, which is how the shared HTTP client reaches
/// it. The raw-socket dialler follows the same setting, so it must not send the tunnel
/// request, with the proxy credentials in it, to that proxy in plaintext.
#[tokio::test]
async fn the_raw_socket_dialler_never_sends_an_https_proxy_its_credentials_in_plaintext() {
    let (address, listening) = listener().await.expect("a loopback port");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let proxy_task = tokio::spawn(async move {
        let (mut stream, _) = listening.accept().await?;
        let mut chunk = [0u8; 1024];
        loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok::<(), io::Error>(());
            }
            let is_done = {
                let mut recorded = recorder.lock();
                recorded.extend_from_slice(chunk.get(..read).unwrap_or_default());
                recorded.first() != Some(&b'C') || recorded.ends_with(b"\r\n\r\n")
            };
            if is_done {
                return Ok(());
            }
        }
    });

    let proxy = ProxySetting::Http { url: format!("https://user:secret@{address}") };
    HttpClient::new(Some(&proxy)).expect("the shared http client takes an https proxy");
    let _ = dialler(proxy)
        .expect("a pinned dialler")
        .connect(&target().expect("an electrum endpoint"))
        .await
        .expect_err("the fake proxy closes the socket");
    proxy_task.abort();

    let sent = String::from_utf8_lossy(&seen.lock()).into_owned();
    // Base64 of `user:secret`, the RFC 7617 basic credentials.
    assert!(
        !sent.contains("dXNlcjpzZWNyZXQ=") && !sent.starts_with("CONNECT "),
        "an https proxy should only ever see a tls handshake, but the tunnel request and \
         the proxy credentials crossed the wire in plaintext: {sent:?}"
    );
}

/// A route the user chose is never silently dropped.
///
/// An HTTP proxy address whose scheme the shared client cannot speak, here a mistyped
/// `http`, must fail the request rather than send it straight to the endpoint from the
/// user's own address.
#[tokio::test]
async fn an_http_proxy_with_an_unusable_scheme_is_never_bypassed_by_the_http_client() {
    let (target_address, target) = listener().await.expect("a loopback port");
    let (proxy_address, proxy_listener) = listener().await.expect("a loopback port");
    let reached = Arc::new(AtomicBool::new(false));
    let target_task = tokio::spawn({
        let reached = Arc::clone(&reached);
        async move {
            let (mut stream, _) = target.accept().await?;
            reached.store(true, Ordering::SeqCst);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .await
        }
    });
    let proxy_task = tokio::spawn(async move {
        while let Ok((stream, _)) = proxy_listener.accept().await {
            drop(stream);
        }
    });

    let proxy = ProxySetting::Http { url: format!("htpp://{proxy_address}") };
    let endpoint =
        Endpoint::parse(ChainId::Bitcoin, Transport::FeeApi, &format!("http://{target_address}"))
            .expect("a loopback endpoint");
    if let Ok(http) = HttpClient::new(Some(&proxy)) {
        let request = http.send(&endpoint, "", HttpRequest::get(Duration::from_secs(5)));
        let _ = timeout(Duration::from_secs(10), request).await;
    }
    target_task.abort();
    proxy_task.abort();

    assert!(
        !reached.load(Ordering::SeqCst),
        "with a proxy configured, the request should go through it or fail, but it went \
         straight to the endpoint"
    );
}

/// The shared HTTP client authenticates to a proxy whose address carries credentials.
///
/// The raw sockets behind Electrum and websockets must present them too, or the one proxy
/// setting works for some chains and refuses the rest.
#[tokio::test]
async fn an_http_proxy_address_with_credentials_authenticates_with_them() {
    let (address, listening) = listener().await.expect("a loopback port");
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let proxy_task = http_proxy(
        listening,
        Arc::clone(&seen),
        "HTTP/1.1 407 Proxy Authentication Required\r\n\r\n",
    );

    let proxy = ProxySetting::Http { url: format!("http://user:secret@{address}") };
    let _ = dialler(proxy)
        .expect("a pinned dialler")
        .connect(&target().expect("an electrum endpoint"))
        .await
        .expect_err("the fake proxy refuses the tunnel");

    let head = String::from_utf8(seen.lock().clone()).expect("an ascii request head");
    // Base64 of `user:secret`, the RFC 7617 basic credentials.
    let has_credentials =
        head.lines().filter_map(|line| line.split_once(':')).any(|(name, value)| {
            name.eq_ignore_ascii_case("proxy-authorization")
                && value.trim() == "Basic dXNlcjpzZWNyZXQ="
        });
    assert!(has_credentials, "{head}");
    proxy_task.await.expect("the proxy task ran").expect("the proxy read the whole request");
}
