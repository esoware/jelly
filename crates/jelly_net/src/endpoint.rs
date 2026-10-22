use std::fmt;
use std::net::IpAddr;

use jelly_core::{ChainId, EndpointLabel, Transport, wire_enum};
use url::Url;

use crate::error::NetError;

const fn scheme_of(transport: Transport, is_plaintext: bool) -> &'static str {
    match (transport, is_plaintext) {
        (Transport::ElectrumSsl, _) => "ssl",
        (Transport::WsJsonrpc, false) => "wss",
        (Transport::WsJsonrpc, true) => "ws",
        (_, false) => "https",
        (_, true) => "http",
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Where a resolved endpoint came from. Declaration order is preference
    /// order among equally healthy endpoints.
    pub enum EndpointOrigin {
        Pinned => "pinned",
        User => "user",
        Default => "default",
    }
}

/// Whether an address without TLS is acceptable.
///
/// A loopback address is always allowed, because a node on the same machine has
/// no network to be intercepted on. Anything else needs an explicit decision by
/// the user, which is what [`PlaintextPolicy::Allowed`] records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaintextPolicy {
    LoopbackOnly,
    Allowed,
}

/// Why a URL was refused. The rejected text is never echoed, because a user endpoint
/// can carry an API key in its query string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UrlRejection {
    Empty,
    Unparsable,
    MissingHost,
    UnsupportedScheme,
    PlaintextNotAllowed,
    MissingPort,
    UnusedTransport,
}

impl UrlRejection {
    pub const ALL: [Self; 7] = [
        Self::Empty,
        Self::Unparsable,
        Self::MissingHost,
        Self::UnsupportedScheme,
        Self::PlaintextNotAllowed,
        Self::MissingPort,
        Self::UnusedTransport,
    ];

    /// The sentence shown to the user. It never repeats any part of the input.
    pub const fn message(self) -> &'static str {
        match self {
            Self::Empty => "Enter an endpoint URL.",
            Self::Unparsable => "Enter a valid endpoint URL.",
            Self::MissingHost => "Include a hostname in the URL.",
            Self::UnsupportedScheme => "Use a URL scheme supported by this connection type.",
            Self::PlaintextNotAllowed => {
                "Use an encrypted connection or allow insecure access for this endpoint."
            }
            Self::MissingPort => "Include a port in the Electrum URL.",
            Self::UnusedTransport => "This network does not use this kind of endpoint.",
        }
    }
}

impl fmt::Display for UrlRejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

/// Canonical endpoint URL used as the key for health, pacing, connections, and stored
/// overrides. Credentials and query parameters remain part of this identity.
///
/// Use only the redacted `label` in logs, errors, and events, and `Debug` shows the label
/// for the same reason. The host is read from that label rather than duplicated, and
/// `port` is the label's with the scheme's default filled in.
#[derive(Clone, PartialEq, Eq)]
pub struct Endpoint {
    chain: ChainId,
    transport: Transport,
    origin: EndpointOrigin,
    url: String,
    input: String,
    note: Option<String>,
    is_plaintext: bool,
    label: EndpointLabel,
    port: u16,
}

impl Endpoint {
    /// Parses `input` for `transport`, refusing plaintext outside loopback.
    pub fn parse(chain: ChainId, transport: Transport, input: &str) -> Result<Self, NetError> {
        Self::parse_with(chain, transport, input, PlaintextPolicy::LoopbackOnly)
    }

    /// Parses `input` under an explicit plaintext policy. Electrum is TLS only
    /// whatever the policy says.
    pub(crate) fn parse_with(
        chain: ChainId,
        transport: Transport,
        input: &str,
        plaintext: PlaintextPolicy,
    ) -> Result<Self, NetError> {
        let parts = canonical_parts(input, transport, plaintext)
            .map_err(|reason| NetError::InvalidUrl { reason })?;
        Ok(Self {
            chain,
            transport,
            origin: EndpointOrigin::Default,
            url: parts.url,
            input: input.trim().to_owned(),
            note: None,
            is_plaintext: parts.is_plaintext,
            label: EndpointLabel::new(parts.scheme, &parts.host, parts.named_port),
            port: parts.port,
        })
    }

    #[must_use]
    pub(crate) fn with_input(mut self, input: &str) -> Self {
        input.trim().clone_into(&mut self.input);
        self
    }

    #[must_use]
    pub(crate) fn with_note(mut self, note: Option<String>) -> Self {
        self.note = note;
        self
    }

    #[must_use]
    pub const fn with_origin(mut self, origin: EndpointOrigin) -> Self {
        self.origin = origin;
        self
    }

    pub const fn chain(&self) -> ChainId {
        self.chain
    }

    pub const fn transport(&self) -> Transport {
        self.transport
    }

    pub const fn origin(&self) -> EndpointOrigin {
        self.origin
    }

    /// The canonical URL, which keys everything stored about this endpoint and is the
    /// address an HTTP client is given.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// What the user typed, preserved for display only.
    pub fn input(&self) -> &str {
        &self.input
    }

    /// The curated note shipped with a default endpoint, if it has one.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// True when the endpoint is reached without TLS. Only a loopback address
    /// or an explicit [`PlaintextPolicy::Allowed`] can produce one.
    pub const fn is_plaintext(&self) -> bool {
        self.is_plaintext
    }

    /// The only representation of this endpoint permitted in an error, a log, or an
    /// event.
    pub const fn label(&self) -> &EndpointLabel {
        &self.label
    }

    /// Host and port for a raw socket, without an IPv6 literal's brackets. An address
    /// that names no port takes its scheme's default.
    pub(crate) fn host_port(&self) -> (&str, u16) {
        (self.label.host(), self.port)
    }

    /// The canonical URL with `path` appended, collapsing the slash between them, and
    /// with `query` merged into the query the endpoint already carries. Meaningful for
    /// the HTTP transports only.
    ///
    /// Build every address through this, so a query string that holds an API key is never
    /// dropped. The path goes first and the query last, because a canonical URL may
    /// already carry a query, and a path appended after it would land in the query and
    /// send the request to the wrong route.
    pub(crate) fn join(&self, path: &str, query: &str) -> String {
        let (base, carried) = self.url.split_once('?').unwrap_or((self.url.as_str(), ""));
        let width =
            self.url.len().saturating_add(path.len()).saturating_add(query.len()).saturating_add(2);
        let mut url = String::with_capacity(width);
        url.push_str(base.trim_end_matches('/'));
        if !path.is_empty() {
            if !path.starts_with('/') {
                url.push('/');
            }
            url.push_str(path);
        }
        for (index, part) in
            [carried, query].into_iter().filter(|part| !part.is_empty()).enumerate()
        {
            url.push(if index == 0 { '?' } else { '&' });
            url.push_str(part);
        }
        url
    }
}

impl fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Endpoint")
            .field("chain", &self.chain)
            .field("transport", &self.transport)
            .field("origin", &self.origin)
            .field("label", &self.label)
            .field("is_plaintext", &self.is_plaintext)
            .finish_non_exhaustive()
    }
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.label, f)
    }
}

/// Canonicalizes `input` to a lowercase host, the transport's own scheme, no default
/// port, no trailing slash, and no fragment.
///
/// This and [`Endpoint::parse_with`] are the workspace's only URL canonicalization, and
/// anything that keys on an endpoint keys on the result.
pub(crate) fn canonicalize(
    input: &str,
    transport: Transport,
    plaintext: PlaintextPolicy,
) -> Result<String, UrlRejection> {
    canonical_parts(input, transport, plaintext).map(|parts| parts.url)
}

struct CanonicalParts {
    url: String,
    scheme: &'static str,
    host: String,
    /// The port the canonical URL names, which is none when it is the scheme's default.
    named_port: Option<u16>,
    port: u16,
    is_plaintext: bool,
}

fn canonical_parts(
    input: &str,
    transport: Transport,
    plaintext: PlaintextPolicy,
) -> Result<CanonicalParts, UrlRejection> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(UrlRejection::Empty);
    }
    let parsed = Url::parse(trimmed).map_err(|_| UrlRejection::Unparsable)?;

    // The parser has already lowercased the scheme.
    let is_plaintext = match parsed.scheme() {
        "https" | "wss" | "ssl" | "tls" => false,
        "http" | "ws" => true,
        _ => return Err(UrlRejection::UnsupportedScheme),
    };
    // Electrum is TLS only, even on loopback, so the scheme decides before the
    // plaintext policy gets a say.
    if is_plaintext && transport == Transport::ElectrumSsl {
        return Err(UrlRejection::UnsupportedScheme);
    }

    let raw_host = parsed.host_str().ok_or(UrlRejection::MissingHost)?;
    let host = raw_host.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
    if host.is_empty() {
        return Err(UrlRejection::MissingHost);
    }
    if is_plaintext && plaintext == PlaintextPolicy::LoopbackOnly && !is_loopback(&host) {
        return Err(UrlRejection::PlaintextNotAllowed);
    }

    let scheme = scheme_of(transport, is_plaintext);
    let named_port = normalized_port(&parsed, scheme);
    let Some(port) = named_port.or_else(|| default_port(scheme)) else {
        return Err(UrlRejection::MissingPort);
    };

    let mut url = String::with_capacity(trimmed.len().saturating_add(8));
    url.push_str(scheme);
    url.push_str("://");
    if !parsed.username().is_empty() || parsed.password().is_some() {
        url.push_str(parsed.username());
        if let Some(password) = parsed.password() {
            url.push(':');
            url.push_str(password);
        }
        url.push('@');
    }
    if host.contains(':') {
        url.push('[');
        url.push_str(&host);
        url.push(']');
    } else {
        url.push_str(&host);
    }
    if let Some(named_port) = named_port {
        url.push(':');
        url.push_str(&named_port.to_string());
    }
    url.push_str(parsed.path().trim_end_matches('/'));
    if let Some(query) = parsed.query() {
        url.push('?');
        url.push_str(query);
    }

    Ok(CanonicalParts { url, scheme, host, named_port, port, is_plaintext })
}

/// The port an address means when it names none. Electrum's `ssl` has no default, which
/// is why an Electrum address must name its port.
fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "https" | "wss" => Some(443),
        "http" | "ws" => Some(80),
        _ => None,
    }
}

/// Drops the port when it is the canonical scheme's default, so two spellings
/// of one endpoint collapse to one key.
fn normalized_port(parsed: &Url, scheme: &str) -> Option<u16> {
    let port = parsed.port_or_known_default()?;
    if Some(port) == default_port(scheme) { None } else { Some(port) }
}

/// A name other than `localhost` can resolve anywhere however it is spelled, so only a
/// literal loopback address counts.
fn is_loopback(host: &str) -> bool {
    host == "localhost" || host.parse::<IpAddr>().is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(url: &str) -> Result<String, UrlRejection> {
        canonicalize(url, Transport::HttpJsonrpc, PlaintextPolicy::LoopbackOnly)
    }

    fn electrum(url: &str) -> Result<String, UrlRejection> {
        canonicalize(url, Transport::ElectrumSsl, PlaintextPolicy::LoopbackOnly)
    }

    #[test]
    fn case_and_trailing_slashes_collapse_to_one_key() {
        for spelling in [
            "https://example.com/eth",
            "https://Example.COM/eth",
            "HTTPS://EXAMPLE.com/eth/",
            "https://example.com:443/eth//",
        ] {
            let canonical = http(spelling).expect(spelling);
            assert_eq!(canonical, "https://example.com/eth", "{spelling}");
        }
    }

    #[test]
    fn a_default_port_is_dropped_and_a_named_one_is_kept() {
        assert_eq!(http("https://example.com:443").expect("443"), "https://example.com");
        assert_eq!(http("https://example.com:8545").expect("8545"), "https://example.com:8545");
        assert_eq!(http("http://127.0.0.1:80").expect("loopback"), "http://127.0.0.1");
    }

    #[test]
    fn a_query_string_is_part_of_the_identity() {
        let canonical = http("https://solana.leorpc.com/?api_key=FREE").expect("a keyed url");
        assert_eq!(canonical, "https://solana.leorpc.com?api_key=FREE");
    }

    #[test]
    fn socket_schemes_normalize_per_transport() {
        assert_eq!(electrum("tls://example.com:50002").expect("tls"), "ssl://example.com:50002");
        assert_eq!(electrum("SSL://Example.com:50002").expect("ssl"), "ssl://example.com:50002");
        assert_eq!(http("wss://example.com").expect("wss"), "https://example.com");
        assert_eq!(http("ssl://example.com:8545").expect("ssl"), "https://example.com:8545");
    }

    #[test]
    fn a_websocket_address_keeps_its_scheme_and_drops_its_default_port() {
        let websocket =
            |url: &str| canonicalize(url, Transport::WsJsonrpc, PlaintextPolicy::LoopbackOnly);
        assert_eq!(websocket("wss://example.com:443").expect("wss"), "wss://example.com");
        assert_eq!(websocket("wss://Example.com/ws/").expect("wss"), "wss://example.com/ws");
        assert_eq!(
            websocket("https://example.com").expect("a secure scheme folds in"),
            "wss://example.com"
        );
        assert_eq!(
            websocket("ws://127.0.0.1:8545").expect("loopback plaintext"),
            "ws://127.0.0.1:8545"
        );

        let refused = websocket("ws://example.com").expect_err("plaintext outside loopback");
        assert_eq!(refused, UrlRejection::PlaintextNotAllowed);
    }

    #[test]
    fn plaintext_needs_loopback_or_permission() {
        let refused = http("http://example.com").expect_err("plaintext");
        assert_eq!(refused, UrlRejection::PlaintextNotAllowed);

        assert_eq!(http("http://127.0.0.1:18081").expect("loopback"), "http://127.0.0.1:18081");
        assert_eq!(http("http://localhost:8545").expect("localhost"), "http://localhost:8545");

        let allowed =
            canonicalize("http://example.com", Transport::MoneroRpc, PlaintextPolicy::Allowed)
                .expect("insecure was allowed");
        assert_eq!(allowed, "http://example.com");
    }

    #[test]
    fn an_electrum_address_must_be_tls_and_must_name_a_port() {
        let no_port = electrum("ssl://example.com").expect_err("no port");
        assert_eq!(no_port, UrlRejection::MissingPort);

        let plaintext = electrum("http://example.com:50002").expect_err("plaintext");
        assert_eq!(plaintext, UrlRejection::UnsupportedScheme);

        // Not even with permission, because the transport itself is TLS only.
        let permitted = canonicalize(
            "http://127.0.0.1:50002",
            Transport::ElectrumSsl,
            PlaintextPolicy::Allowed,
        )
        .expect_err("electrum is tls only");
        assert_eq!(permitted, UrlRejection::UnsupportedScheme);
    }

    #[test]
    fn an_ipv6_literal_keeps_its_brackets_in_the_url_and_loses_them_for_the_socket() {
        let endpoint =
            Endpoint::parse(ChainId::Bitcoin, Transport::ElectrumSsl, "ssl://[2001:db8::1]:50002")
                .expect("an ipv6 electrum address");

        assert_eq!(endpoint.url(), "ssl://[2001:db8::1]:50002");
        assert_eq!(endpoint.host_port(), ("2001:db8::1", 50002));
        assert_eq!(endpoint.label().to_string(), "ssl://[2001:db8::1]:50002");
    }

    #[test]
    fn a_socket_address_without_a_port_takes_its_schemes_default() {
        let secure = Endpoint::parse(ChainId::Ethereum, Transport::WsJsonrpc, "wss://ws.example")
            .expect("a websocket endpoint");
        assert_eq!(secure.host_port(), ("ws.example", 443));

        let plain = Endpoint::parse(ChainId::Ethereum, Transport::WsJsonrpc, "ws://127.0.0.1")
            .expect("a loopback websocket endpoint");
        assert_eq!(plain.host_port(), ("127.0.0.1", 80));
    }

    #[test]
    fn an_endpoint_joins_a_path_onto_its_canonical_url() {
        let endpoint = Endpoint::parse(
            ChainId::Monero,
            Transport::MoneroRpc,
            "https://xmr-node.cakewallet.com:18081",
        )
        .expect("a monero node");

        assert_eq!(endpoint.url(), "https://xmr-node.cakewallet.com:18081");
        assert_eq!(
            endpoint.join("/json_rpc", ""),
            "https://xmr-node.cakewallet.com:18081/json_rpc"
        );
        assert_eq!(
            endpoint.join("get_blocks.bin", ""),
            "https://xmr-node.cakewallet.com:18081/get_blocks.bin"
        );
        assert_eq!(endpoint.join("", ""), endpoint.url());
        assert!(!endpoint.is_plaintext());
        assert_eq!(endpoint.transport(), Transport::MoneroRpc);
        assert_eq!(endpoint.origin(), EndpointOrigin::Default);
        assert_eq!(endpoint.chain(), ChainId::Monero);
    }

    #[test]
    fn a_path_goes_in_front_of_the_query_the_endpoint_already_carries() {
        let keyed = Endpoint::parse(
            ChainId::Monero,
            Transport::MoneroRpc,
            "https://node.example:18081/rpc?api_key=SECRET",
        )
        .expect("a keyed node");

        assert_eq!(keyed.url(), "https://node.example:18081/rpc?api_key=SECRET");
        assert_eq!(
            keyed.join("/json_rpc", ""),
            "https://node.example:18081/rpc/json_rpc?api_key=SECRET"
        );
        assert_eq!(keyed.join("", ""), keyed.url());
    }

    #[test]
    fn a_query_is_merged_into_the_one_the_endpoint_already_carries() {
        let keyed = Endpoint::parse(
            ChainId::Monero,
            Transport::MoneroRpc,
            "https://node.example:18081/rpc?api_key=SECRET",
        )
        .expect("a keyed node");
        assert_eq!(
            keyed.join("", "module=account"),
            "https://node.example:18081/rpc?api_key=SECRET&module=account"
        );

        let plain =
            Endpoint::parse(ChainId::Ethereum, Transport::HistoryApi, "https://api.example/v2")
                .expect("a rest endpoint");
        assert_eq!(plain.join("", "module=account"), "https://api.example/v2?module=account");
        assert_eq!(plain.join("/api", ""), "https://api.example/v2/api");
    }
}
