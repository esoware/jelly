use std::error::Error as StdError;
use std::time::Duration;

use jelly_core::{
    BoxedSource, BuildError, ChainError, ChainId, EndpointAttempt, EndpointLabel, FailureKind,
    duration_millis,
};
use tokio::time::timeout;

use crate::endpoint::UrlRejection;
use crate::retry_after::RetryAfter;

/// Endpoints appear only as an [`EndpointLabel`], so no userinfo, path, or query reaches a
/// message, a log, an event, or a stored health record.
///
/// Redaction happens at construction, so even `Debug` output is safe to share.
#[derive(Debug, thiserror::Error)]
pub enum NetError {
    /// A transport failure. A protocol crate can box its own error as `source` and carry
    /// it through a rotation, then get it back with [`NetError::source_of`].
    #[error("{endpoint}: {}", kind.as_str())]
    Endpoint {
        endpoint: EndpointLabel,
        kind: FailureKind,
        #[source]
        source: BoxedSource,
    },
    #[error("{endpoint}: timed out after {elapsed_millis} ms")]
    Timeout { endpoint: EndpointLabel, elapsed_millis: u64 },
    /// `detail` is a fixed string, so no library's message reaches the interface. Log the
    /// library's error separately when it helps.
    #[error("{endpoint}: malformed response: {detail}")]
    Protocol { endpoint: EndpointLabel, detail: &'static str },
    /// Every host would send the same bytes, so a caller whose answer scales with its
    /// query, such as a log read, narrows the query instead of rotating.
    #[error("{endpoint}: the response is larger than {max_bytes} bytes")]
    TooLarge { endpoint: EndpointLabel, max_bytes: usize },
    #[error("{endpoint}: http {status}")]
    Status { endpoint: EndpointLabel, status: u16 },
    #[error("{endpoint}: rate limited")]
    RateLimited { endpoint: EndpointLabel, retry_after: Option<RetryAfter> },
    #[error("{endpoint}: rpc error {code}: {message}")]
    Rpc { endpoint: EndpointLabel, code: i64, message: String },
    #[error("{endpoint}: serves {found}, not {expected}")]
    WrongNetwork { endpoint: EndpointLabel, expected: String, found: String },
    #[error("{}: no endpoint is configured", chain.display_name())]
    NoEndpoints { chain: ChainId },
    /// No endpoint could be tried: each was held back by its pacer, disqualified for
    /// serving another network, or past the walk's budget. Nothing was sent, so no
    /// endpoint's health moved.
    #[error("{}: no endpoint was ready for {operation}", chain.display_name())]
    NoneReady { chain: ChainId, operation: &'static str },
    #[error("{}: no endpoint answered for {operation}", chain.display_name())]
    AllFailed { chain: ChainId, operation: &'static str, attempts: Vec<EndpointAttempt> },
    #[error("invalid endpoint url: {reason}")]
    InvalidUrl { reason: UrlRejection },
    #[error("the endpoint defaults for {} are unreadable: {detail}", chain.display_name())]
    Defaults { chain: ChainId, detail: &'static str },
    #[error("{context}")]
    Config {
        context: &'static str,
        #[source]
        source: BoxedSource,
    },
}

impl NetError {
    /// A transport failure. `source` must already be free of any URL.
    pub fn endpoint<E: StdError + Send + Sync + 'static>(
        endpoint: &EndpointLabel,
        kind: FailureKind,
        source: E,
    ) -> Self {
        Self::Endpoint { endpoint: endpoint.clone(), kind, source: Box::new(source) }
    }

    pub fn protocol(endpoint: &EndpointLabel, detail: &'static str) -> Self {
        Self::Protocol { endpoint: endpoint.clone(), detail }
    }

    pub fn timed_out(endpoint: &EndpointLabel, elapsed: Duration) -> Self {
        Self::Timeout { endpoint: endpoint.clone(), elapsed_millis: duration_millis(elapsed) }
    }

    pub fn config<E: StdError + Send + Sync + 'static>(context: &'static str, source: E) -> Self {
        Self::Config { context, source: Box::new(source) }
    }

    /// The failure class that attempts and logs carry, so nothing downstream has to read
    /// a message to classify it.
    pub const fn kind(&self) -> FailureKind {
        match *self {
            Self::Endpoint { kind, .. } => kind,
            Self::Timeout { .. } => FailureKind::Timeout,
            Self::Protocol { .. }
            | Self::TooLarge { .. }
            | Self::WrongNetwork { .. }
            | Self::Rpc { .. } => FailureKind::Protocol,
            Self::Status { status, .. } => {
                if status >= 500 {
                    FailureKind::Server
                } else {
                    FailureKind::Client
                }
            }
            Self::RateLimited { .. } | Self::NoneReady { .. } => FailureKind::RateLimited,
            Self::NoEndpoints { .. }
            | Self::AllFailed { .. }
            | Self::InvalidUrl { .. }
            | Self::Defaults { .. }
            | Self::Config { .. } => FailureKind::Client,
        }
    }

    /// Whether nothing could be reached, as opposed to an endpoint answering badly. An
    /// endpoint serving another network and a chain with no endpoint both count.
    pub fn is_unreachable(&self) -> bool {
        match self {
            Self::Endpoint { kind, .. } => kind.is_unreachable(),
            Self::AllFailed { attempts, .. } => {
                !attempts.is_empty() && attempts.iter().all(|attempt| attempt.kind.is_unreachable())
            }
            Self::Timeout { .. } | Self::WrongNetwork { .. } | Self::NoEndpoints { .. } => true,
            Self::Protocol { .. }
            | Self::TooLarge { .. }
            | Self::Status { .. }
            | Self::RateLimited { .. }
            | Self::Rpc { .. }
            | Self::NoneReady { .. }
            | Self::InvalidUrl { .. }
            | Self::Defaults { .. }
            | Self::Config { .. } => false,
        }
    }

    /// The endpoint this failure belongs to. The aggregate variants name a chain instead.
    pub const fn label(&self) -> Option<&EndpointLabel> {
        match self {
            Self::Endpoint { endpoint, .. }
            | Self::Timeout { endpoint, .. }
            | Self::Protocol { endpoint, .. }
            | Self::TooLarge { endpoint, .. }
            | Self::Status { endpoint, .. }
            | Self::RateLimited { endpoint, .. }
            | Self::Rpc { endpoint, .. }
            | Self::WrongNetwork { endpoint, .. } => Some(endpoint),
            Self::NoEndpoints { .. }
            | Self::NoneReady { .. }
            | Self::AllFailed { .. }
            | Self::InvalidUrl { .. }
            | Self::Defaults { .. }
            | Self::Config { .. } => None,
        }
    }

    /// How long the endpoint asked to be left alone, when it said so.
    pub const fn retry_after(&self) -> Option<RetryAfter> {
        match *self {
            Self::RateLimited { retry_after, .. } => retry_after,
            Self::Endpoint { .. }
            | Self::Timeout { .. }
            | Self::Protocol { .. }
            | Self::TooLarge { .. }
            | Self::Status { .. }
            | Self::Rpc { .. }
            | Self::WrongNetwork { .. }
            | Self::NoEndpoints { .. }
            | Self::NoneReady { .. }
            | Self::AllFailed { .. }
            | Self::InvalidUrl { .. }
            | Self::Defaults { .. }
            | Self::Config { .. } => None,
        }
    }

    /// The typed cause a protocol crate boxed into [`NetError::Endpoint`] or
    /// [`NetError::Config`], so it gets its own error back after a walk.
    pub fn source_of<E: StdError + Send + Sync + 'static>(&self) -> Option<&E> {
        match *self {
            Self::Endpoint { ref source, .. } | Self::Config { ref source, .. } => {
                source.downcast_ref::<E>()
            }
            Self::Timeout { .. }
            | Self::Protocol { .. }
            | Self::TooLarge { .. }
            | Self::Status { .. }
            | Self::RateLimited { .. }
            | Self::Rpc { .. }
            | Self::WrongNetwork { .. }
            | Self::NoEndpoints { .. }
            | Self::NoneReady { .. }
            | Self::AllFailed { .. }
            | Self::InvalidUrl { .. }
            | Self::Defaults { .. } => None,
        }
    }

    /// The owned form of [`NetError::source_of`]. The error is returned
    /// unchanged when it carries no cause of that type.
    pub fn into_source_of<E: StdError + Send + Sync + 'static>(self) -> Result<E, Self> {
        match self {
            Self::Endpoint { endpoint, kind, source } => match source.downcast::<E>() {
                Ok(typed) => Ok(*typed),
                Err(source) => Err(Self::Endpoint { endpoint, kind, source }),
            },
            Self::Config { context, source } => match source.downcast::<E>() {
                Ok(typed) => Ok(*typed),
                Err(source) => Err(Self::Config { context, source }),
            },
            other @ (Self::Timeout { .. }
            | Self::Protocol { .. }
            | Self::TooLarge { .. }
            | Self::Status { .. }
            | Self::RateLimited { .. }
            | Self::Rpc { .. }
            | Self::WrongNetwork { .. }
            | Self::NoEndpoints { .. }
            | Self::NoneReady { .. }
            | Self::AllFailed { .. }
            | Self::InvalidUrl { .. }
            | Self::Defaults { .. }) => Err(other),
        }
    }
}

/// Runs `future` for at most `budget`, and reports running out as `label` timing out.
pub(crate) async fn within<T>(
    label: &EndpointLabel,
    budget: Duration,
    future: impl Future<Output = Result<T, NetError>>,
) -> Result<T, NetError> {
    timeout(budget, future).await.unwrap_or_else(|_| Err(NetError::timed_out(label, budget)))
}

/// A failed walk, a chain with no endpoints, or a walk where none was ready is
/// endpoints-down, and a configuration failure is internal. Everything else is one
/// endpoint's answer.
impl From<NetError> for ChainError {
    fn from(error: NetError) -> Self {
        match error {
            NetError::AllFailed { chain, operation, attempts } => {
                Self::endpoints_down(chain, operation, attempts)
            }
            NetError::NoEndpoints { chain } => {
                Self::endpoints_down(chain, "no endpoint configured", Vec::new())
            }
            NetError::NoneReady { chain, operation } => {
                Self::endpoints_down(chain, operation, Vec::new())
            }
            other @ (NetError::Endpoint { .. }
            | NetError::Timeout { .. }
            | NetError::Protocol { .. }
            | NetError::TooLarge { .. }
            | NetError::Status { .. }
            | NetError::RateLimited { .. }
            | NetError::Rpc { .. }
            | NetError::WrongNetwork { .. }) => Self::rpc(other),
            // A URL in use already passed `resolve` and the defaults are compiled in, so
            // either failing is a bug in this build. An unusable proxy isn't a node's
            // answer either.
            other @ (NetError::InvalidUrl { .. }
            | NetError::Defaults { .. }
            | NetError::Config { .. }) => Self::internal("the network configuration", other),
        }
    }
}

/// A network failure while building an adapter is a failed build. The pool helpers on
/// `Network` report an empty rotation themselves.
impl From<NetError> for BuildError {
    fn from(error: NetError) -> Self {
        Self::Failed(ChainError::from(error))
    }
}
