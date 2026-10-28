use std::fmt;
use std::net::Ipv6Addr;

use serde::{Serialize, Serializer};

use crate::chain::ChainId;
use crate::wire_enum;

/// Redacted endpoint identity for errors, logs, events, and health records. It keeps only
/// the scheme, host, and port, since userinfo, paths, and queries can carry credentials.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointLabel {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl EndpointLabel {
    /// Strips path, query, and fragment before removing userinfo. Searching for `@` first
    /// could mistake an API key in the query for part of the host.
    pub fn new(scheme: &str, after_scheme: &str, port: Option<u16>) -> Self {
        let authority = after_scheme.split(['/', '?', '#']).next().unwrap_or(after_scheme);
        let bare = authority.rsplit_once('@').map_or(authority, |(_, rest)| rest);
        Self {
            scheme: scheme.trim().to_ascii_lowercase(),
            host: bare.trim().to_ascii_lowercase(),
            port,
        }
    }

    /// The label of a URL in whatever spelling it was stored or typed. Without a scheme
    /// separator the whole string is the authority.
    ///
    /// Any port stays inside `host` and `port` is `None`, so this label differs from a
    /// `new` label of the same endpoint that names its port separately.
    pub fn from_url(url: &str) -> Self {
        let (scheme, rest) = url.split_once("://").unwrap_or(("", url));
        Self::new(scheme, rest, None)
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The bare host for a label from `new`, but host and port for one from `from_url`.
    pub fn host(&self) -> &str {
        &self.host
    }
}

impl fmt::Display for EndpointLabel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://", self.scheme)?;
        // A `from_url` label keeps any port inside `host`, so only a bare IPv6 literal is
        // bracketed rather than every host with a colon.
        if self.host.parse::<Ipv6Addr>().is_ok() {
            write!(f, "[{}]", self.host)?;
        } else {
            f.write_str(&self.host)?;
        }
        match self.port {
            Some(port) => write!(f, ":{port}"),
            None => Ok(()),
        }
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Network failure classes assigned by `jelly_net`. Downstream code must not reclassify
    /// error messages.
    pub enum FailureKind {
        Connect => "connect",
        Tls => "tls",
        Timeout => "timeout",
        Transport => "transport",
        Protocol => "protocol",
        RateLimited => "rate_limited",
        Server => "server",
        Client => "client",
    }
}

impl FailureKind {
    /// Whether the endpoint was never reached: it wouldn't connect, finish TLS, or answer
    /// in time. Every other class is an answer, however unusable.
    pub const fn is_unreachable(self) -> bool {
        match self {
            Self::Connect | Self::Tls | Self::Timeout | Self::Transport => true,
            Self::Protocol | Self::RateLimited | Self::Server | Self::Client => false,
        }
    }
}

/// One failed endpoint attempt. `message` is a log field that never crosses IPC, and
/// callers must redact it before constructing this.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EndpointAttempt {
    /// The endpoint's label as text, such as `https://node.example.com:8443`.
    #[cfg_attr(feature = "ipc", specta(type = String))]
    #[serde(serialize_with = "label_text")]
    pub endpoint: EndpointLabel,
    pub kind: FailureKind,
    #[serde(skip)]
    pub message: String,
}

impl EndpointAttempt {
    pub fn new(endpoint: EndpointLabel, kind: FailureKind, message: impl Into<String>) -> Self {
        Self { endpoint, kind, message: message.into() }
    }
}

fn label_text<S: Serializer>(label: &EndpointLabel, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.collect_str(label)
}

/// One endpoint the user added or flagged, as the engine stores it and the endpoint
/// registry applies it.
///
/// Both URLs can carry credentials, so `Debug` leaves them out.
#[derive(Clone, PartialEq, Eq)]
pub struct EndpointOverride {
    /// The canonical URL, as the store holds it.
    pub url: String,
    /// What the user typed.
    pub display: String,
    pub is_enabled: bool,
    pub is_pinned: bool,
}

#[cfg(feature = "test-util")]
impl EndpointOverride {
    pub fn new(url: impl Into<String>) -> Self {
        let url = url.into();
        Self { url: url.clone(), display: url, is_enabled: true, is_pinned: false }
    }
}

impl fmt::Debug for EndpointOverride {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EndpointOverride")
            .field("is_enabled", &self.is_enabled)
            .field("is_pinned", &self.is_pinned)
            .finish_non_exhaustive()
    }
}

/// What's remembered about one endpoint between runs, as the network layer records it and
/// the engine stores it.
///
/// Nothing here reads the monotonic clock, so the record stays meaningful across a
/// restart. That's why a quarantine deadline is never part of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointHealth {
    pub score_permille: u16,
    pub successes: u64,
    pub failures: u64,
    pub consecutive_failures: u32,
    pub latency_ms: Option<u64>,
    pub last_success_unix_secs: Option<i64>,
    pub last_failure_unix_secs: Option<i64>,
    /// The last failure's already-redacted message.
    pub last_error: Option<String>,
}

impl EndpointHealth {
    /// The score of an endpoint nothing has been heard from, and the one a stored score
    /// decays back toward.
    pub const BASELINE_PERMILLE: u16 = 750;
}

impl Default for EndpointHealth {
    fn default() -> Self {
        Self {
            score_permille: Self::BASELINE_PERMILLE,
            successes: 0,
            failures: 0,
            consecutive_failures: 0,
            latency_ms: None,
            last_success_unix_secs: None,
            last_failure_unix_secs: None,
            last_error: None,
        }
    }
}

/// One endpoint's health as it was last persisted, so ordering survives a restart.
///
/// `Debug` shows the URL only as its [`EndpointLabel`].
#[derive(Clone, PartialEq, Eq)]
pub struct EndpointHealthRecord {
    pub chain: ChainId,
    pub url: String,
    pub health: EndpointHealth,
}

impl fmt::Debug for EndpointHealthRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EndpointHealthRecord")
            .field("chain", &self.chain)
            .field("label", &EndpointLabel::from_url(&self.url))
            .field("health", &self.health)
            .finish()
    }
}
