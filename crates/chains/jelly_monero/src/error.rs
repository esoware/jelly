use jelly_core::{
    AddressError, Amount, AssetId, ChainError, ChainId, CoreError, EndpointLabel, EntropyError,
    FailureKind, KeyKind, Rejection,
};
use jelly_net::NetError;
use monero_wallet::send::SendError;

use crate::params::{CHAIN, FEE_RATE_UNIT, MAX_HARDFORK_VERSION};

/// A node's own wording appears only in a classified [`Rejection`].
///
/// Every endpoint inside is an [`EndpointLabel`], so a `Debug` print is safe to
/// log, and no variant carries a fragment of an address, a key, or a scan blob.
#[derive(Debug, thiserror::Error)]
pub enum MoneroError {
    #[error(transparent)]
    Net(NetError),
    /// A node refused a broadcast, classified once.
    #[error("{0}")]
    Rejected(Rejection),
    /// The node answered, and the answer cannot be used. Attributed to that
    /// endpoint so the walk rotates.
    #[error("{endpoint}: {what}")]
    Answer { endpoint: EndpointLabel, what: &'static str },
    /// The node quoted a fee this build refuses to pay.
    #[error(
        "{endpoint}: the quoted fee of {piconero_per_weight} piconero per weight unit is above the ceiling of {ceiling_piconero_per_weight}"
    )]
    FeeCeiling {
        endpoint: EndpointLabel,
        piconero_per_weight: u64,
        ceiling_piconero_per_weight: u64,
    },
    #[error(
        "monero hardfork v{version} at height {height} is newer than this build reads (v{MAX_HARDFORK_VERSION})"
    )]
    ProtocolTooNew { version: u8, height: u64 },
    /// Named at every call site rather than derived, so no other failure that
    /// happens to be a [`CoreError`] is reported as a storage one.
    #[error("monero wallet state could not be read or written")]
    Cursor(#[source] CoreError),
    /// The scan blob or the sends blob, read back and refused.
    #[error("a persisted monero blob is unusable: {what}")]
    Blob { what: &'static str },
    #[error("monero wallet state is too large to persist: {what}")]
    TooLargeToPersist { what: &'static str },
    #[error("the block scan did not finish")]
    ScanTask,
    #[error("monero block {height} a node served does not scan")]
    UnscannableBlock { height: u64 },

    #[error("{} is not monero", chain.display_name())]
    ForeignChain { chain: ChainId },
    #[error("monero carries no tokens, so it cannot send {}", asset.symbol())]
    ForeignAsset { asset: AssetId },
    #[error("Choose a Monero fee level or fee rate.")]
    ForeignFeeSelection,
    #[error("Enter the fee rate as a whole number of piconero per byte.")]
    FeeRateUnreadable,
    #[error(
        "The fee rate of {rate} {FEE_RATE_UNIT} is below the network minimum of {minimum} {FEE_RATE_UNIT}."
    )]
    FeeRateBelowMinimum { rate: u64, minimum: u64 },
    #[error("The fee rate can be at most {maximum} {FEE_RATE_UNIT}.")]
    FeeRateAboveMaximum { maximum: u64 },
    #[error("This address is no longer available in this wallet.")]
    NotWatched { address_id: i64 },
    #[error("Choose one address to send from.")]
    NotOneSource,
    /// The detail comes from the address codec, whose stated contract is that no
    /// message it produces carries a fragment of its input.
    #[error("invalid monero address")]
    Address(#[source] AddressError),

    #[error("The amount exceeds Monero's limit.")]
    AmountTooLarge,
    /// Carried as typed amounts rather than as text, so the interface formats
    /// them with the asset's own decimals.
    #[error(
        "{} needed and only {} unlocked in {}",
        needed.format(asset.decimals()),
        available.format(asset.decimals()),
        asset.symbol()
    )]
    InsufficientFunds { asset: AssetId, needed: Amount, available: Amount },
    #[error("This needs more than {maximum} inputs. Split it into smaller payments.")]
    TooManyInputs { maximum: usize },
    #[error("monero block height {height} is out of range")]
    HeightOutOfRange { height: u64 },
    #[error("a node answered a request for monero block {expected} with block {found}")]
    BlockRange { expected: u64, found: u64 },
    /// The library's own message goes to the log, never into this error.
    #[error("ring selection failed")]
    Decoys,

    #[error("that unsigned transaction cannot be read: {detail}")]
    Payload { detail: &'static str },
    #[error("the ring was chosen {age} blocks ago, past the limit of {maximum}")]
    PayloadStale { age: u64, maximum: u64 },
    #[error("that unsigned transaction belongs to another monero account")]
    PayloadForeignAccount,
    #[error("the signed transaction cannot be read: {detail}")]
    SignedTx { detail: &'static str },
    #[error("that transaction was not signed in this session and holds no input reservation")]
    NotSignedHere,

    #[error("monero key material: {what}")]
    Key { what: &'static str },
    /// The operating system's entropy source refused. There is no userspace
    /// fallback, because a transaction whose ephemeral secrets are not random is
    /// worse than one that was never built.
    #[error("the system entropy source is unavailable")]
    Entropy {
        #[source]
        source: EntropyError,
    },
    #[error("a monero spend key was expected, not a {}", got.as_str())]
    WrongKeyKind { got: KeyKind },
    #[error("a monero spend key is 32 bytes, not {got}")]
    WrongKeyLength { got: usize },
    #[error("that spend key does not belong to the account this transaction spends from")]
    KeyNotOwner,
    #[error("the transaction could not be assembled")]
    Build(#[source] SendError),
    #[error("the transaction could not be signed")]
    Sign(#[source] SendError),

    #[error("internal: {context}")]
    Internal { context: &'static str },
}

impl MoneroError {
    pub(crate) fn answer(endpoint: &EndpointLabel, what: &'static str) -> Self {
        Self::Answer { endpoint: endpoint.clone(), what }
    }

    const fn label(&self) -> Option<&EndpointLabel> {
        match *self {
            Self::Net(ref inner) => inner.label(),
            Self::Answer { ref endpoint, .. } | Self::FeeCeiling { ref endpoint, .. } => {
                Some(endpoint)
            }
            Self::Rejected(_)
            | Self::ProtocolTooNew { .. }
            | Self::Cursor(_)
            | Self::Blob { .. }
            | Self::TooLargeToPersist { .. }
            | Self::ScanTask
            | Self::UnscannableBlock { .. }
            | Self::ForeignChain { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::FeeRateUnreadable
            | Self::FeeRateBelowMinimum { .. }
            | Self::FeeRateAboveMaximum { .. }
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::Address(_)
            | Self::AmountTooLarge
            | Self::InsufficientFunds { .. }
            | Self::TooManyInputs { .. }
            | Self::HeightOutOfRange { .. }
            | Self::BlockRange { .. }
            | Self::Decoys
            | Self::Payload { .. }
            | Self::PayloadStale { .. }
            | Self::PayloadForeignAccount
            | Self::SignedTx { .. }
            | Self::NotSignedHere
            | Self::Key { .. }
            | Self::Entropy { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::Build(_)
            | Self::Sign(_)
            | Self::Internal { .. } => None,
        }
    }

    /// Whether a build that failed this way failed at the node its ring is pinned
    /// to, so the next build must not be pinned to that node too. A build asks
    /// no other node anything, so a transport failure or an unusable answer can
    /// only have come from that one.
    pub(crate) const fn is_ring_node_fault(&self) -> bool {
        match *self {
            Self::Net(_) | Self::Answer { .. } | Self::Decoys => true,
            Self::Rejected(_)
            | Self::FeeCeiling { .. }
            | Self::ProtocolTooNew { .. }
            | Self::Cursor(_)
            | Self::Blob { .. }
            | Self::TooLargeToPersist { .. }
            | Self::ScanTask
            | Self::UnscannableBlock { .. }
            | Self::ForeignChain { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::FeeRateUnreadable
            | Self::FeeRateBelowMinimum { .. }
            | Self::FeeRateAboveMaximum { .. }
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::Address(_)
            | Self::AmountTooLarge
            | Self::InsufficientFunds { .. }
            | Self::TooManyInputs { .. }
            | Self::HeightOutOfRange { .. }
            | Self::BlockRange { .. }
            | Self::Payload { .. }
            | Self::PayloadStale { .. }
            | Self::PayloadForeignAccount
            | Self::SignedTx { .. }
            | Self::NotSignedHere
            | Self::Key { .. }
            | Self::Entropy { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::Build(_)
            | Self::Sign(_)
            | Self::Internal { .. } => false,
        }
    }

    /// Whether the node asked could not be reached at all, which is the only failure
    /// that sends a broadcast transaction's follow-up to another node.
    pub(crate) fn is_unreachable(&self) -> bool {
        matches!(self, Self::Net(inner) if inner.is_unreachable())
    }

    /// The class the health tracker records. Nothing re-derives it from a
    /// message.
    const fn kind(&self) -> FailureKind {
        match *self {
            Self::Net(ref inner) => inner.kind(),
            Self::Answer { .. }
            | Self::FeeCeiling { .. }
            | Self::BlockRange { .. }
            | Self::ProtocolTooNew { .. }
            | Self::Rejected(_) => FailureKind::Protocol,
            Self::Cursor(_)
            | Self::Blob { .. }
            | Self::TooLargeToPersist { .. }
            | Self::ScanTask
            | Self::UnscannableBlock { .. }
            | Self::ForeignChain { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::FeeRateUnreadable
            | Self::FeeRateBelowMinimum { .. }
            | Self::FeeRateAboveMaximum { .. }
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::Address(_)
            | Self::AmountTooLarge
            | Self::InsufficientFunds { .. }
            | Self::TooManyInputs { .. }
            | Self::HeightOutOfRange { .. }
            | Self::Decoys
            | Self::Payload { .. }
            | Self::PayloadStale { .. }
            | Self::PayloadForeignAccount
            | Self::SignedTx { .. }
            | Self::NotSignedHere
            | Self::Key { .. }
            | Self::Entropy { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::Build(_)
            | Self::Sign(_)
            | Self::Internal { .. } => FailureKind::Client,
        }
    }
}

/// A `NetError` carrying one of ours came from a closure inside the pool, so it
/// is recovered rather than wrapped twice.
impl From<NetError> for MoneroError {
    fn from(error: NetError) -> Self {
        error.into_source_of::<Self>().unwrap_or_else(Self::Net)
    }
}

/// Puts a Monero failure into the shape the pool classifies, keeping the
/// original as the source so the typed value survives the round trip.
impl From<MoneroError> for NetError {
    fn from(error: MoneroError) -> Self {
        if let MoneroError::Net(net) = error {
            return net;
        }
        let Some(endpoint) = error.label().cloned() else {
            return Self::config("monero", error);
        };
        let kind = error.kind();
        Self::endpoint(&endpoint, kind, error)
    }
}

/// The interface's vocabulary. The match has no wildcard, so a new variant has
/// to be classified here.
impl From<MoneroError> for ChainError {
    fn from(error: MoneroError) -> Self {
        match error {
            MoneroError::Net(inner) => inner.into(),
            MoneroError::Rejected(rejection) => Self::Rejected(rejection),
            MoneroError::Cursor(source) => Self::internal("monero wallet state storage", source),
            MoneroError::InsufficientFunds { asset, needed, available } => {
                Self::InsufficientFunds { asset, needed, available }
            }
            MoneroError::ProtocolTooNew { version, .. } => {
                Self::protocol_too_new(CHAIN, version.to_string())
            }

            blob @ MoneroError::Blob { .. } => Self::internal("monero wallet blob", blob),
            too_large @ MoneroError::TooLargeToPersist { .. } => {
                Self::internal("monero wallet state", too_large)
            }
            MoneroError::ScanTask => {
                Self::internal_without_source("the monero block scan did not finish")
            }

            error @ (MoneroError::Answer { .. }
            | MoneroError::FeeCeiling { .. }
            | MoneroError::BlockRange { .. }
            | MoneroError::HeightOutOfRange { .. }
            | MoneroError::UnscannableBlock { .. }
            | MoneroError::Decoys) => Self::rpc(error),

            error @ (MoneroError::ForeignChain { .. } | MoneroError::ForeignAsset { .. }) => {
                Self::unsupported(error)
            }

            error @ MoneroError::Address(_) => Self::invalid_address(error),

            error @ (MoneroError::ForeignFeeSelection
            | MoneroError::FeeRateUnreadable
            | MoneroError::FeeRateBelowMinimum { .. }
            | MoneroError::FeeRateAboveMaximum { .. }
            | MoneroError::NotWatched { .. }
            | MoneroError::NotOneSource
            | MoneroError::AmountTooLarge
            | MoneroError::TooManyInputs { .. }) => Self::invalid_input(error),

            error @ (MoneroError::Key { .. }
            | MoneroError::WrongKeyKind { .. }
            | MoneroError::WrongKeyLength { .. }
            | MoneroError::KeyNotOwner
            | MoneroError::Payload { .. }
            | MoneroError::PayloadStale { .. }
            | MoneroError::PayloadForeignAccount
            | MoneroError::SignedTx { .. }
            | MoneroError::NotSignedHere
            | MoneroError::Build(_)
            | MoneroError::Sign(_)) => Self::signing(error),

            MoneroError::Entropy { source } => Self::internal("system entropy", source),
            MoneroError::Internal { context } => Self::internal_without_source(context),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transport_failure_round_trips_through_the_pool_unchanged() {
        let original = NetError::NoEndpoints { chain: CHAIN };
        let ours = MoneroError::from(original);
        assert!(matches!(ours, MoneroError::Net(NetError::NoEndpoints { .. })));
        let back = NetError::from(ours);
        assert!(matches!(back, NetError::NoEndpoints { .. }));
    }
}
