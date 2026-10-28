use std::env;
use std::io;
use std::net::Ipv6Addr;

use jelly_core::{EndpointLabel, FailureKind, ProxySetting};
use reqwest::ClientBuilder;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;
use url::Url;

use crate::credentials::Credentials;
use crate::dialer::Socket;
use crate::error::NetError;
use crate::tls;

const UNUSABLE: &str = "the proxy address is not usable";

/// The environment variable a raw socket reads for [`ProxySetting::System`], in both
/// spellings, as curl and the shared HTTP client do.
const SYSTEM_PROXY_VARS: [&str; 2] = ["ALL_PROXY", "all_proxy"];

const MAX_CONNECT_HEAD_BYTES: usize = 8 * 1024;

/// Routes the shared HTTP client.
///
/// No proxy means a direct connection, enforced with `no_proxy` so a proxy the
/// environment names can't pick the traffic up after the user chose direct.
pub(crate) fn route_client(
    builder: ClientBuilder,
    proxy: Option<&ProxySetting>,
) -> Result<ClientBuilder, NetError> {
    let Some(proxy) = proxy else {
        return Ok(builder.no_proxy());
    };
    let Some(url) = named_url(proxy)? else {
        return Ok(builder);
    };
    let proxy =
        reqwest::Proxy::all(url.as_str()).map_err(|source| NetError::config(UNUSABLE, source))?;
    Ok(builder.proxy(proxy))
}

/// The address a named proxy setting means, or `None` for [`ProxySetting::System`].
///
/// The shared HTTP client and the raw-socket dialer both route by this one reading, so
/// one setting can't take two routes.
///
/// A SOCKS address is always respelled `socks5h`, a bare `host:port` included, because
/// reqwest resolves `socks5` names on this machine and Tor needs the proxy to resolve
/// them. An HTTP address without a scheme is `http`, as reqwest reads one. Any scheme but
/// `http` or `https` on an HTTP address is refused, because reqwest would skip a proxy it
/// can't speak and connect directly.
fn named_url(proxy: &ProxySetting) -> Result<Option<Url>, NetError> {
    let unusable = |source| NetError::config(UNUSABLE, source);
    let parsed = match proxy {
        ProxySetting::System => return Ok(None),
        ProxySetting::Socks5h { url } => {
            let trimmed = url.trim();
            let unschemed = trimmed.split_once("://").map_or(trimmed, |(_, rest)| rest);
            Url::parse(&format!("socks5h://{unschemed}")).map_err(unusable)?
        }
        ProxySetting::Http { url } => {
            let trimmed = url.trim();
            let parsed = if trimmed.contains("://") {
                Url::parse(trimmed)
            } else {
                Url::parse(&format!("http://{trimmed}"))
            };
            let parsed = parsed.map_err(unusable)?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(NetError::config(
                    UNUSABLE,
                    io::Error::from(io::ErrorKind::Unsupported),
                ));
            }
            parsed
        }
    };
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(unusable(url::ParseError::EmptyHost));
    }
    Ok(Some(parsed))
}

/// A stream to `host:port`, tunneled through `proxy` when there is one.
///
/// A configured proxy is never bypassed. If it can't be used, the connection fails.
pub(crate) async fn route_socket(
    proxy: Option<&ProxySetting>,
    label: &EndpointLabel,
    host: &str,
    port: u16,
) -> Result<Box<dyn Socket>, NetError> {
    let Some(route) = proxy.map(proxy_route).transpose()?.flatten() else {
        return Ok(Box::new(dial(label, host, port).await?));
    };
    let hop = dial(label, &route.host, route.port).await?;
    let mut stream: Box<dyn Socket> = match route.protocol {
        ProxyProtocol::Socks5 | ProxyProtocol::HttpConnect => Box::new(hop),
        ProxyProtocol::HttpsConnect => Box::new(proxy_tls(label, &route.host, hop).await?),
    };
    let credentials = route.credentials.as_ref();
    match route.protocol {
        ProxyProtocol::Socks5 => {
            socks5_connect(label, stream.as_mut(), host, port, credentials).await?;
        }
        ProxyProtocol::HttpConnect | ProxyProtocol::HttpsConnect => {
            http_connect(label, stream.as_mut(), host, port, credentials).await?;
        }
    }
    Ok(stream)
}

/// How a proxy is asked to open a tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProxyProtocol {
    Socks5,
    HttpConnect,
    /// HTTP CONNECT inside TLS to the proxy, as the shared HTTP client reaches an `https`
    /// proxy address.
    HttpsConnect,
}

impl ProxyProtocol {
    const fn default_port(self) -> u16 {
        match self {
            Self::Socks5 => 1080,
            Self::HttpConnect => 80,
            Self::HttpsConnect => 443,
        }
    }
}

/// One resolved proxy hop, with where to dial and what to say once connected.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProxyRoute {
    host: String,
    port: u16,
    protocol: ProxyProtocol,
    credentials: Option<Credentials>,
}

/// The route a raw socket takes, where `None` is a direct connection.
///
/// `None` comes only from [`ProxySetting::System`] when the environment names no usable
/// proxy. A named proxy that won't parse is an error, so a route the user chose is never
/// silently dropped.
fn proxy_route(proxy: &ProxySetting) -> Result<Option<ProxyRoute>, NetError> {
    let Some(url) = named_url(proxy)? else {
        return Ok(system_proxy());
    };
    route_of(&url).map(Some).ok_or_else(|| NetError::config(UNUSABLE, url::ParseError::EmptyHost))
}

/// What [`ProxySetting::System`] resolves to for a raw socket. Only `ALL_PROXY` is read,
/// though the shared HTTP client also reads `HTTP_PROXY` and `HTTPS_PROXY`.
fn system_proxy() -> Option<ProxyRoute> {
    let value = SYSTEM_PROXY_VARS.into_iter().find_map(|name| env::var(name).ok())?;
    parse_system_proxy(&value)
}

/// Reads one `ALL_PROXY` value. Anything that isn't a proxy address this dialer can
/// speak counts as no setting at all.
fn parse_system_proxy(value: &str) -> Option<ProxyRoute> {
    let trimmed = value.trim();
    let parsed = if trimmed.contains("://") {
        Url::parse(trimmed)
    } else {
        Url::parse(&format!("http://{trimmed}"))
    };
    parsed.ok().as_ref().and_then(route_of)
}

/// The hop a parsed proxy address names, or `None` when it names no host or a scheme
/// this dialer can't speak.
fn route_of(parsed: &Url) -> Option<ProxyRoute> {
    let protocol = match parsed.scheme() {
        // A raw socket always sends the target as a name, so socks5 and socks5h are
        // the same request and the proxy resolves either way.
        "socks5" | "socks5h" => ProxyProtocol::Socks5,
        "http" => ProxyProtocol::HttpConnect,
        "https" => ProxyProtocol::HttpsConnect,
        _ => return None,
    };
    let host = parsed.host_str().filter(|host| !host.is_empty())?;
    Some(ProxyRoute {
        host: host.trim_start_matches('[').trim_end_matches(']').to_owned(),
        port: parsed.port_or_known_default().unwrap_or_else(|| protocol.default_port()),
        protocol,
        credentials: Credentials::of(parsed),
    })
}

/// No timeout here, because the callers bound the whole connection with one budget.
async fn dial(label: &EndpointLabel, host: &str, port: u16) -> Result<TcpStream, NetError> {
    let stream = TcpStream::connect((host, port))
        .await
        .map_err(|source| NetError::endpoint(label, FailureKind::Connect, source))?;
    // Request and response are latency bound, not throughput bound.
    let _ = stream.set_nodelay(true);
    Ok(stream)
}

/// The TLS an `https` proxy address asks for, validated against the compiled-in roots as
/// the shared HTTP client validates it.
async fn proxy_tls(
    label: &EndpointLabel,
    proxy_host: &str,
    stream: TcpStream,
) -> Result<TlsStream<TcpStream>, NetError> {
    let name = ServerName::try_from(proxy_host.to_owned())
        .map_err(|source| NetError::endpoint(label, FailureKind::Tls, source))?;
    TlsConnector::from(tls::ca_validated_client_config()?)
        .connect(name, stream)
        .await
        .map_err(|source| NetError::endpoint(label, FailureKind::Tls, source))
}

/// Opens a SOCKS5 CONNECT tunnel, RFC 1928, with username and password authentication,
/// RFC 1929, when the proxy address carries credentials.
///
/// The target is always sent as a name, never an address, so the proxy resolves it as
/// `socks5h` promises. It offers the one method the shared HTTP client would offer for
/// the same address.
///
/// ```text
/// greeting | 0x05 | nmethods=1 | 0x00 none or 0x02 password |
/// reply    | 0x05 | method                                  |
/// auth     | 0x01 | ulen | username | plen | password       |
/// reply    | 0x01 | status                                  |
/// request  | 0x05 | 0x01 CONNECT | 0x00 | 0x03 name | len | name | port be |
/// reply    | 0x05 | status | 0x00 | atyp | addr | port      |
/// ```
async fn socks5_connect(
    label: &EndpointLabel,
    stream: &mut dyn Socket,
    host: &str,
    port: u16,
    credentials: Option<&Credentials>,
) -> Result<(), NetError> {
    let name = host.as_bytes();
    let name_length = u8::try_from(name.len())
        .map_err(|_| NetError::protocol(label, "the proxy target host is too long"))?;

    let method = if credentials.is_some() { 0x02 } else { 0x00 };
    write_all(label, stream, &[0x05, 0x01, method]).await?;
    let mut greeting = [0u8; 2];
    read_exact(label, stream, &mut greeting).await?;
    if greeting != [0x05, method] {
        let detail = if credentials.is_some() {
            "the proxy refused password authentication"
        } else {
            "the proxy refused an unauthenticated connection"
        };
        return Err(NetError::protocol(label, detail));
    }

    if let Some(credentials) = credentials {
        let username = credentials.username.as_bytes();
        let password = credentials.password.as_bytes();
        let too_long = |_| NetError::protocol(label, "the proxy credentials are too long");
        let username_length = u8::try_from(username.len()).map_err(too_long)?;
        let password_length = u8::try_from(password.len()).map_err(too_long)?;
        let mut auth =
            Vec::with_capacity(username.len().saturating_add(password.len()).saturating_add(3));
        auth.extend_from_slice(&[0x01, username_length]);
        auth.extend_from_slice(username);
        auth.push(password_length);
        auth.extend_from_slice(password);
        write_all(label, stream, &auth).await?;
        let mut verdict = [0u8; 2];
        read_exact(label, stream, &mut verdict).await?;
        if verdict != [0x01, 0x00] {
            return Err(NetError::protocol(label, "the proxy refused the credentials"));
        }
    }

    let mut request = Vec::with_capacity(name.len().saturating_add(7));
    request.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, name_length]);
    request.extend_from_slice(name);
    request.extend_from_slice(&port.to_be_bytes());
    write_all(label, stream, &request).await?;

    let mut head = [0u8; 4];
    read_exact(label, stream, &mut head).await?;
    let [version, reply, _, address_type] = head;
    if version != 0x05 || reply != 0x00 {
        return Err(NetError::protocol(label, "the proxy refused the connect request"));
    }
    let trailing = match address_type {
        0x01 => 6usize,
        0x04 => 18usize,
        0x03 => {
            let mut length_byte = [0u8; 1];
            read_exact(label, stream, &mut length_byte).await?;
            let [name_length] = length_byte;
            usize::from(name_length).saturating_add(2)
        }
        _ => {
            return Err(NetError::protocol(
                label,
                "the proxy replied with an unknown address type",
            ));
        }
    };
    let mut sink = vec![0u8; trailing];
    read_exact(label, stream, &mut sink).await
}

/// Opens an HTTP CONNECT tunnel, RFC 9110 section 9.3.6, with RFC 7617 basic credentials
/// when the proxy address carries them.
///
/// The target is sent as a name, so the proxy resolves it. TLS normally runs inside the
/// tunnel, so a plaintext proxy learns only the host name.
async fn http_connect(
    label: &EndpointLabel,
    stream: &mut dyn Socket,
    host: &str,
    port: u16,
    credentials: Option<&Credentials>,
) -> Result<(), NetError> {
    let authority = if host.parse::<Ipv6Addr>().is_ok() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    let authorization = credentials.map_or_else(String::new, |credentials| {
        format!("Proxy-Authorization: {}\r\n", credentials.basic())
    });
    let request = format!(
        "CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n{authorization}Proxy-Connection: keep-alive\r\n\r\n"
    );
    write_all(label, stream, request.as_bytes()).await?;

    // One byte at a time, because the TLS handshake follows the head on this socket and
    // a buffered reader could swallow its first record.
    let mut head: Vec<u8> = Vec::with_capacity(128);
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if head.len() >= MAX_CONNECT_HEAD_BYTES {
            return Err(NetError::protocol(label, "the proxy sent an oversized reply"));
        }
        read_exact(label, stream, &mut byte).await?;
        head.extend_from_slice(&byte);
    }

    // The status is the second field of the first line, as in
    // `HTTP/1.1 200 Connection established`, and any 2xx is a tunnel.
    let text = String::from_utf8_lossy(&head);
    let status = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok())
        .ok_or_else(|| NetError::protocol(label, "the proxy reply has no status"))?;
    if !matches!(status, 200..300) {
        return Err(NetError::protocol(label, "the proxy refused the connect request"));
    }
    Ok(())
}

/// Flushes too, because TLS to an `https` proxy can hold written bytes back, and the
/// proxy answers only once the whole request has arrived.
async fn write_all(
    label: &EndpointLabel,
    stream: &mut dyn Socket,
    bytes: &[u8],
) -> Result<(), NetError> {
    let written = async {
        stream.write_all(bytes).await?;
        stream.flush().await
    };
    written.await.map_err(|source| NetError::endpoint(label, FailureKind::Connect, source))
}

async fn read_exact(
    label: &EndpointLabel,
    stream: &mut dyn Socket,
    into: &mut [u8],
) -> Result<(), NetError> {
    match stream.read_exact(into).await {
        Ok(_) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::UnexpectedEof => {
            Err(NetError::protocol(label, "the proxy closed the connection"))
        }
        Err(source) => Err(NetError::endpoint(label, FailureKind::Connect, source)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(host: &str, port: u16, protocol: ProxyProtocol) -> ProxyRoute {
        ProxyRoute { host: host.to_owned(), port, protocol, credentials: None }
    }

    #[test]
    fn a_system_proxy_variable_is_read_as_the_route_it_names() {
        assert_eq!(
            parse_system_proxy("socks5h://127.0.0.1:9050"),
            Some(route("127.0.0.1", 9050, ProxyProtocol::Socks5))
        );
        // socks5 and socks5h are one route for a raw socket, because the target always
        // goes to the proxy as a name.
        assert_eq!(
            parse_system_proxy("socks5://127.0.0.1:9050"),
            Some(route("127.0.0.1", 9050, ProxyProtocol::Socks5))
        );
        assert_eq!(
            parse_system_proxy("  http://proxy.example:8080  "),
            Some(route("proxy.example", 8080, ProxyProtocol::HttpConnect))
        );
    }

    #[test]
    fn a_system_proxy_without_a_scheme_is_an_http_route() {
        for value in ["proxy.example:8080", "  proxy.example:8080  "] {
            assert_eq!(
                parse_system_proxy(value),
                Some(route("proxy.example", 8080, ProxyProtocol::HttpConnect))
            );
        }
        assert_eq!(
            parse_system_proxy("[::1]:8080"),
            Some(route("::1", 8080, ProxyProtocol::HttpConnect))
        );
    }

    #[test]
    fn a_variable_naming_nothing_this_dialer_speaks_is_no_setting() {
        for value in ["", "   ", "ftp://proxy.example", "socks4://p:1080"] {
            assert_eq!(parse_system_proxy(value), None, "{value}");
        }
    }

    #[test]
    fn a_named_proxy_without_a_port_takes_its_protocols_default() {
        let socks = ProxySetting::Socks5h { url: "socks5h://tor.example".to_owned() };
        assert_eq!(
            proxy_route(&socks).expect("a socks proxy"),
            Some(route("tor.example", 1080, ProxyProtocol::Socks5))
        );

        let http = ProxySetting::Http { url: "http://proxy.example".to_owned() };
        assert_eq!(
            proxy_route(&http).expect("an http proxy"),
            Some(route("proxy.example", 80, ProxyProtocol::HttpConnect))
        );

        let bracketed = ProxySetting::Socks5h { url: "socks5h://[::1]:9050".to_owned() };
        assert_eq!(
            proxy_route(&bracketed).expect("an ipv6 proxy"),
            Some(route("::1", 9050, ProxyProtocol::Socks5))
        );
    }

    #[test]
    fn credentials_in_a_proxy_address_are_decoded_and_never_printed() {
        let keyed =
            ProxySetting::Http { url: "http://us%40er:p%3Ass@proxy.example:8080".to_owned() };
        let route = proxy_route(&keyed).expect("an http proxy").expect("a named route");
        assert_eq!(
            route.credentials,
            Some(Credentials { username: "us@er".to_owned(), password: "p:ss".to_owned() })
        );
        let printed = format!("{route:?}");
        assert!(!printed.contains("us@er") && !printed.contains("p:ss"), "{printed}");

        let named = ProxySetting::Socks5h { url: "socks5h://tor@127.0.0.1:9050".to_owned() };
        let route = proxy_route(&named).expect("a socks proxy").expect("a named route");
        assert_eq!(
            route.credentials,
            Some(Credentials { username: "tor".to_owned(), password: String::new() })
        );
    }

    #[test]
    fn an_unusable_named_proxy_is_refused_rather_than_bypassed() {
        let broken = ProxySetting::Socks5h { url: "not a url".to_owned() };
        let refused = proxy_route(&broken);
        assert!(matches!(refused, Err(NetError::Config { context: UNUSABLE, .. })), "{refused:?}");
    }

    #[test]
    fn a_named_address_without_a_scheme_takes_its_routes_own() {
        let socks = ProxySetting::Socks5h { url: "127.0.0.1:9050".to_owned() };
        assert_eq!(
            proxy_route(&socks).expect("a socks proxy"),
            Some(route("127.0.0.1", 9050, ProxyProtocol::Socks5))
        );

        let http = ProxySetting::Http { url: "proxy.example:8080".to_owned() };
        assert_eq!(
            proxy_route(&http).expect("an http proxy"),
            Some(route("proxy.example", 8080, ProxyProtocol::HttpConnect))
        );
    }

    #[test]
    fn an_https_proxy_address_is_reached_over_tls() {
        let named = ProxySetting::Http { url: "https://proxy.example".to_owned() };
        assert_eq!(
            proxy_route(&named).expect("an https proxy"),
            Some(route("proxy.example", 443, ProxyProtocol::HttpsConnect))
        );
        assert_eq!(
            parse_system_proxy("https://proxy.example:8443"),
            Some(route("proxy.example", 8443, ProxyProtocol::HttpsConnect))
        );
    }

    #[test]
    fn an_http_route_naming_another_scheme_is_refused_on_every_transport() {
        for url in ["htpp://proxy.example", "socks5://proxy.example:1080", "ftp://proxy.example"] {
            let named = ProxySetting::Http { url: url.to_owned() };
            proxy_route(&named).expect_err(url);
            route_client(ClientBuilder::new(), Some(&named)).expect_err(url);
        }
    }
}
