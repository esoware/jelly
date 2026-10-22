use std::time::Duration;

use bitcoin::BlockHash;
use jelly_core::{
    ChainError, ChainId, EndpointLabel, FailureKind, Rejection, RetryPolicy, duration_millis,
};
use jelly_net::{NetError, Outcome};

use crate::method::Method;
use crate::version::ProtocolVersion;

/// Every endpoint inside is an [`EndpointLabel`], redacted at construction, so
/// no address, userinfo, or query string can reach a message or a `Debug` print.
#[derive(Debug, thiserror::Error)]
pub enum ElectrumError {
    /// A failure from the shared network layer, such as the dial, the TLS
    /// handshake, or an aggregate from a whole rotation.
    #[error(transparent)]
    Net(NetError),
    /// The socket is gone. Nothing that was in flight on it was answered.
    #[error("{endpoint}: the connection is closed")]
    Closed { endpoint: EndpointLabel },
    #[error("{endpoint}: {method} timed out after {elapsed_millis} ms")]
    Timeout { endpoint: EndpointLabel, method: Method, elapsed_millis: u64 },
    /// The server answered with something this build cannot read or accept. The
    /// detail is a fixed reason, so no library's message can reach the interface.
    #[error("{endpoint}: {method}: {detail}")]
    Protocol { endpoint: EndpointLabel, method: Method, detail: &'static str },
    /// The server answered with a JSON-RPC error member, as an object or a bare
    /// string.
    #[error("{endpoint}: {method}: server error {code}: {message}")]
    Rpc { endpoint: EndpointLabel, method: Method, code: i64, message: String },
    #[error("{endpoint}: {method}: a batch of {requested} was answered with {received}")]
    BatchLength { endpoint: EndpointLabel, method: Method, requested: usize, received: usize },
    #[error("{endpoint}: the server offered protocol {found}, which this build cannot read")]
    UnreadableVersion { endpoint: EndpointLabel, found: String },
    #[error("{endpoint}: the server offered protocol {found}, older than the {minimum} needed")]
    VersionTooOld { endpoint: EndpointLabel, found: ProtocolVersion, minimum: ProtocolVersion },
    #[error("{endpoint}: the server offered protocol {found}, newer than the {maximum} supported")]
    VersionTooNew { endpoint: EndpointLabel, found: ProtocolVersion, maximum: ProtocolVersion },
    #[error("{endpoint}: {method} needs protocol {since} and the server negotiated {negotiated}")]
    MethodUnavailable {
        endpoint: EndpointLabel,
        method: Method,
        since: ProtocolVersion,
        negotiated: ProtocolVersion,
    },
    /// A socket is on another chain than this client was built for. Every socket
    /// proves its chain's blocks before it serves, so an endpoint that switches
    /// backends mid-session cannot slip in a foreign chain's history.
    ///
    /// The hashes are boxed because two inline would make every `Result` that
    /// carries this error trip `clippy::result_large_err`.
    #[error("{endpoint}: serves block {height} as {found}, not {expected}")]
    WrongChain {
        endpoint: EndpointLabel,
        height: u32,
        expected: Box<BlockHash>,
        found: Box<BlockHash>,
    },
    /// The node refused a broadcast, already classified.
    #[error("{endpoint}: {rejection}")]
    Rejected { endpoint: EndpointLabel, rejection: Rejection },
}

impl ElectrumError {
    pub(crate) fn protocol(endpoint: &EndpointLabel, method: Method, detail: &'static str) -> Self {
        Self::Protocol { endpoint: endpoint.clone(), method, detail }
    }

    pub(crate) fn closed(endpoint: &EndpointLabel) -> Self {
        Self::Closed { endpoint: endpoint.clone() }
    }

    pub(crate) fn timed_out(endpoint: &EndpointLabel, method: Method, elapsed: Duration) -> Self {
        Self::Timeout {
            endpoint: endpoint.clone(),
            method,
            elapsed_millis: duration_millis(elapsed),
        }
    }

    /// Only a wrapped [`NetError`] aggregate has no endpoint.
    pub const fn label(&self) -> Option<&EndpointLabel> {
        match self {
            Self::Net(net) => net.label(),
            Self::Closed { endpoint }
            | Self::Timeout { endpoint, .. }
            | Self::Protocol { endpoint, .. }
            | Self::Rpc { endpoint, .. }
            | Self::BatchLength { endpoint, .. }
            | Self::UnreadableVersion { endpoint, .. }
            | Self::VersionTooOld { endpoint, .. }
            | Self::VersionTooNew { endpoint, .. }
            | Self::MethodUnavailable { endpoint, .. }
            | Self::WrongChain { endpoint, .. }
            | Self::Rejected { endpoint, .. } => Some(endpoint),
        }
    }

    /// The class the health tracker records. Nothing re-derives it from a
    /// message.
    pub const fn kind(&self) -> FailureKind {
        match *self {
            Self::Net(ref net) => net.kind(),
            Self::Closed { .. } => FailureKind::Transport,
            Self::Timeout { .. } => FailureKind::Timeout,
            Self::Protocol { .. }
            | Self::Rpc { .. }
            | Self::BatchLength { .. }
            | Self::UnreadableVersion { .. }
            | Self::VersionTooOld { .. }
            | Self::VersionTooNew { .. }
            | Self::MethodUnavailable { .. }
            | Self::WrongChain { .. }
            | Self::Rejected { .. } => FailureKind::Protocol,
        }
    }

    /// What a read's failover walk does with this failure.
    ///
    /// Everything rotates except a refusal the node has made up its mind about,
    /// which every other node will make the same way, and a method the negotiated
    /// protocol lacks. That one never reached the wire and says nothing about the
    /// server's health, and most public servers negotiate below the protocol that
    /// added it, so rotating would penalise every server in the pool at once. A
    /// broadcast uses [`classify_broadcast`] instead, because a refusal there is
    /// an answer the caller has to see rather than an endpoint failure.
    pub const fn outcome(&self) -> Outcome {
        match *self {
            Self::Rejected { ref rejection, .. } => match rejection.retry {
                RetryPolicy::TryAnotherEndpoint => Outcome::Rotate,
                RetryPolicy::DidNotLand
                | RetryPolicy::FinalEverywhere
                | RetryPolicy::AlreadyLanded => Outcome::Fail,
            },
            Self::MethodUnavailable { .. } => Outcome::Fail,
            Self::Net(_)
            | Self::Closed { .. }
            | Self::Timeout { .. }
            | Self::Protocol { .. }
            | Self::Rpc { .. }
            | Self::BatchLength { .. }
            | Self::UnreadableVersion { .. }
            | Self::VersionTooOld { .. }
            | Self::VersionTooNew { .. }
            | Self::WrongChain { .. } => Outcome::Rotate,
        }
    }

    pub const fn rejection(&self) -> Option<&Rejection> {
        if let Self::Rejected { ref rejection, .. } = *self { Some(rejection) } else { None }
    }

    /// `chain` comes from the caller because nothing in this crate knows which
    /// coin it is talking to.
    pub fn into_chain_error(self, chain: ChainId) -> ChainError {
        match self {
            Self::Net(net) => ChainError::from(net),
            Self::Rejected { rejection, .. } => ChainError::Rejected(rejection),
            Self::VersionTooNew { found, .. } => {
                ChainError::protocol_too_new(chain, found.to_string())
            }
            other @ (Self::Closed { .. }
            | Self::Timeout { .. }
            | Self::Protocol { .. }
            | Self::Rpc { .. }
            | Self::BatchLength { .. }
            | Self::UnreadableVersion { .. }
            | Self::VersionTooOld { .. }
            | Self::MethodUnavailable { .. }
            | Self::WrongChain { .. }) => ChainError::rpc(other),
        }
    }
}

/// Puts an Electrum failure into the shape the pool classifies, keeping the
/// original as the source so the typed value survives the round trip.
impl From<ElectrumError> for NetError {
    fn from(error: ElectrumError) -> Self {
        let endpoint = match error {
            ElectrumError::Net(net) => return net,
            ElectrumError::Closed { ref endpoint }
            | ElectrumError::Timeout { ref endpoint, .. }
            | ElectrumError::Protocol { ref endpoint, .. }
            | ElectrumError::Rpc { ref endpoint, .. }
            | ElectrumError::BatchLength { ref endpoint, .. }
            | ElectrumError::UnreadableVersion { ref endpoint, .. }
            | ElectrumError::VersionTooOld { ref endpoint, .. }
            | ElectrumError::VersionTooNew { ref endpoint, .. }
            | ElectrumError::MethodUnavailable { ref endpoint, .. }
            | ElectrumError::WrongChain { ref endpoint, .. }
            | ElectrumError::Rejected { ref endpoint, .. } => endpoint.clone(),
        };
        let kind = error.kind();
        Self::endpoint(&endpoint, kind, error)
    }
}

/// Recovers the typed failure the pool carried through as a source.
impl From<NetError> for ElectrumError {
    fn from(error: NetError) -> Self {
        error.into_source_of::<Self>().unwrap_or_else(Self::Net)
    }
}

/// The classifier every Electrum read is walked with. It applies
/// [`ElectrumError::outcome`] to a typed Electrum failure and rotates on anything
/// else.
pub fn classify(error: &NetError) -> Outcome {
    error.source_of::<ElectrumError>().map_or(Outcome::Rotate, ElectrumError::outcome)
}

/// The classifier a broadcast is walked with.
///
/// A refusal is an answer, so the attempt counts as a success for the endpoint
/// and the typed rejection reaches the caller instead of being lost in an
/// all-endpoints-failed report.
pub fn classify_broadcast(error: &NetError) -> Outcome {
    if matches!(error.source_of::<ElectrumError>(), Some(ElectrumError::Rejected { .. })) {
        Outcome::Accept
    } else {
        Outcome::Rotate
    }
}
