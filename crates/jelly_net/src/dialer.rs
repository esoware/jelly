use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use jelly_core::{ChainId, Clock, FailureKind, PinStore, ProxySetting};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_rustls::TlsConnector;

use crate::endpoint::Endpoint;
use crate::error::{NetError, within};
use crate::proxy;
use crate::tls;

/// A duplex byte stream a socket transport runs over.
///
/// Direct TCP, a proxy tunnel (inside TLS to an `https` proxy), TLS over either, or an
/// in-memory pipe in a test.
pub trait Socket: AsyncRead + AsyncWrite + Send + Unpin + fmt::Debug + 'static {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin + fmt::Debug + 'static> Socket for T {}

/// Dials Electrum and websocket byte streams through the same proxy setting as the HTTP
/// client.
///
/// The constructor picks the TLS policy: first-use pinning for Electrum, CA validation
/// for websockets.
#[derive(Clone)]
pub struct Dialer {
    connector: TlsConnector,
    proxy: Option<ProxySetting>,
    connect_timeout: Duration,
}

/// Shows only whether there's a proxy, so its address stays out of logs.
impl fmt::Debug for Dialer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Dialer")
            .field("is_proxied", &self.proxy.is_some())
            .field("connect_timeout", &self.connect_timeout)
            .finish_non_exhaustive()
    }
}

impl Dialer {
    /// Trusts a server's key on first use, since most public Electrum servers are
    /// self-signed. Only a certificate that chains to the compiled-in roots may replace a
    /// recorded key.
    pub fn pinned(
        pins: Arc<dyn PinStore>,
        clock: Arc<dyn Clock>,
        chain: ChainId,
        proxy: Option<ProxySetting>,
        connect_timeout: Duration,
    ) -> Result<Self, NetError> {
        let config = tls::pinned_client_config(pins, clock, chain)?;
        Ok(Self { connector: TlsConnector::from(config), proxy, connect_timeout })
    }

    /// Full CA validation against the compiled-in roots, for the websocket transport.
    pub(crate) fn ca_validated(
        proxy: Option<ProxySetting>,
        connect_timeout: Duration,
    ) -> Result<Self, NetError> {
        let config = tls::ca_validated_client_config()?;
        Ok(Self { connector: TlsConnector::from(config), proxy, connect_timeout })
    }

    /// A socket to `endpoint`, inside TLS unless the address was admitted as plaintext
    /// (loopback or the user's explicit choice, and never Electrum).
    ///
    /// Even a loopback address goes through the proxy. `connect_timeout` covers the tunnel
    /// and the TLS handshake as well as the dial, since a host can accept a connection and
    /// then go silent.
    pub async fn connect(&self, endpoint: &Endpoint) -> Result<Box<dyn Socket>, NetError> {
        within(endpoint.label(), self.connect_timeout, self.open(endpoint)).await
    }

    async fn open(&self, endpoint: &Endpoint) -> Result<Box<dyn Socket>, NetError> {
        let label = endpoint.label();
        let (host, port) = endpoint.host_port();
        let tunnel = proxy::route_socket(self.proxy.as_ref(), label, host, port).await?;
        if endpoint.is_plaintext() {
            return Ok(tunnel);
        }
        let name = ServerName::try_from(host.to_owned())
            .map_err(|source| NetError::endpoint(label, FailureKind::Tls, source))?;
        let stream = self
            .connector
            .connect(name, tunnel)
            .await
            .map_err(|source| NetError::endpoint(label, FailureKind::Tls, source))?;
        Ok(Box::new(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialer_never_prints_its_proxy_address() {
        for proxy in [
            ProxySetting::Http {
                url: "http://private-user:private-password@proxy.example".to_owned(),
            },
            ProxySetting::Socks5h {
                url: "socks5h://private-user:private-password@proxy.example".to_owned(),
            },
        ] {
            let dialer = Dialer::ca_validated(Some(proxy), Duration::from_secs(5))
                .expect("a ca validated dialer");
            let rendered = format!("{dialer:?}");
            assert!(!rendered.contains("private-user"), "{rendered}");
            assert!(!rendered.contains("private-password"), "{rendered}");
            assert!(!rendered.contains("proxy.example"), "{rendered}");
        }
    }
}
