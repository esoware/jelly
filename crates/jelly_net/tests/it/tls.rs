use std::io;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{
    ChainId, Clock, FailureKind, PinStore, Transport,
    test_util::{FakeClock, MemoryPinStore},
};
use jelly_net::{Dialer, Endpoint, NetError};
use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use rustls::version::{TLS12, TLS13};
use rustls::{DEFAULT_VERSIONS, ServerConfig, SupportedProtocolVersion};
use tokio::io::AsyncWriteExt as _;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

/// A self-signed X.509 version 1 certificate and its P-256 key in PKCS #8.
///
/// Version 1 has no version field and no extensions. `openssl x509 -req` made
/// certificates of this version before OpenSSL 3.0, and that is the command the
/// `ElectrumX` setup guide gives for a server's own certificate.
const V1_CERTIFICATE: &[u8] = include_bytes!("../fixtures/v1_self_signed.der");
const V1_KEY: &[u8] = include_bytes!("../fixtures/v1_self_signed.key.der");

/// A P-256 key in PKCS #8 that the version 1 certificate doesn't hold.
const OTHER_KEY: &[u8] = include_bytes!("../fixtures/other_p256.key.der");

#[derive(Debug)]
struct OneCertificate(Arc<CertifiedKey>);

impl ResolvesServerCert for OneCertificate {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }
}

/// A loopback TLS server that presents the version 1 certificate to one client, signs its
/// handshake with `key`, speaks only `versions`, and writes a line once the handshake
/// completes.
async fn signing_server(
    key: &[u8],
    versions: &[&'static SupportedProtocolVersion],
) -> io::Result<(u16, JoinHandle<io::Result<()>>)> {
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.to_vec()));
    let signing = ring::sign::any_supported_type(&key).map_err(io::Error::other)?;
    let certified = CertifiedKey::new(vec![CertificateDer::from(V1_CERTIFICATE.to_vec())], signing);
    let config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_protocol_versions(versions)
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(OneCertificate(Arc::new(certified))));
    let acceptor = TlsAcceptor::from(Arc::new(config));
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut tls = acceptor.accept(stream).await?;
        tls.write_all(b"{}\n").await
    });
    Ok((port, server))
}

/// Connects a pinned dialer with an empty pin store to the loopback `port`.
async fn first_use(port: u16) -> Result<(), NetError> {
    let pins: Arc<dyn PinStore> = Arc::new(MemoryPinStore::new());
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::default());
    let dialer = Dialer::pinned(pins, clock, ChainId::Bitcoin, None, Duration::from_secs(5))?;
    let endpoint = Endpoint::parse(
        ChainId::Bitcoin,
        Transport::ElectrumSsl,
        &format!("ssl://127.0.0.1:{port}"),
    )?;
    dialer.connect(&endpoint).await.map(drop)
}

/// A self-signed version 1 certificate is trusted on first use like any other.
///
/// The pin is taken from the key whether or not the certificate carries a version field,
/// so a server whose self-signed certificate is version 1 must be reachable as well.
#[tokio::test]
async fn a_self_signed_version_1_certificate_is_trusted_on_first_use() {
    let (port, server) =
        signing_server(V1_KEY, DEFAULT_VERSIONS).await.expect("a loopback tls server");

    let connected = first_use(port).await;
    server.abort();

    assert!(
        connected.is_ok(),
        "a first-seen self-signed server should be pinned and connected, but the handshake \
         failed: {connected:?}"
    );
}

/// TLS 1.2 checks the handshake signature on a path of its own, which takes a version 1
/// certificate too.
#[tokio::test]
async fn a_self_signed_version_1_certificate_completes_a_tls_1_2_handshake() {
    let (port, server) = signing_server(V1_KEY, &[&TLS12]).await.expect("a loopback tls server");

    let connected = first_use(port).await;
    server.abort();

    assert!(connected.is_ok(), "{connected:?}");
}

/// The pin trusts the certificate's key, so a handshake signed by any other key is
/// refused on either version.
#[tokio::test]
async fn a_handshake_signed_by_a_key_the_certificate_does_not_hold_is_refused() {
    for version in [&TLS12, &TLS13] {
        let (port, server) =
            signing_server(OTHER_KEY, &[version]).await.expect("a loopback tls server");

        let refused = first_use(port).await;
        server.abort();

        assert!(
            matches!(refused, Err(NetError::Endpoint { kind: FailureKind::Tls, .. })),
            "{:?}: {refused:?}",
            version.version
        );
    }
}
