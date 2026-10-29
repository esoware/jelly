use jelly_core::{
    Amount, AssetId, ChainError, ChainId, CoreError, EndpointLabel, KeyError, KeyKind, Rejection,
};
use jelly_net::NetError;

use crate::fee::trx_text;
use crate::route::Route;

/// Only a classified [`Rejection`], [`TronError::Fault`], and
/// [`TronError::CallFailed`] carry a node's own wording.
///
/// Endpoints are always an [`EndpointLabel`], and no variant carries key material,
/// a URL, or a query string, so `Debug` output is safe to log.
#[derive(Debug, thiserror::Error)]
pub enum TronError {
    #[error(transparent)]
    Net(NetError),
    /// The network refused a broadcast.
    #[error("{0}")]
    Rejected(Rejection),
    /// An unreadable answer, reported by position because serde's message quotes
    /// the answer back.
    #[error("{endpoint}: the {route} answer is unreadable at line {line} column {column}")]
    Answer { endpoint: EndpointLabel, route: Route, line: usize, column: usize },
    /// The answer parsed, but a field we need is missing or unusable, such as a
    /// block id that isn't 32 bytes.
    #[error("{endpoint}: the {route} answer has no usable {what}")]
    Shape { endpoint: EndpointLabel, route: Route, what: &'static str },
    /// TRON reports several faults as HTTP 200 with an `Error` member.
    #[error("{endpoint}: {route} was refused: {detail}")]
    Fault { endpoint: EndpointLabel, route: Route, detail: String },
    /// A read-only contract call didn't execute cleanly. The detail is the node's
    /// message.
    #[error("the contract call was refused: {detail}")]
    CallFailed { detail: String },
    /// Not `#[from]`, so no other [`CoreError`] gets reported as a storage
    /// failure.
    #[error("the cursor store failed")]
    Cursor(#[source] CoreError),
    /// The signed bytes aren't tracked, because they weren't signed here or
    /// stopped being tracked first. With no record of the send to write, they're
    /// never broadcast.
    #[error("the signed transaction has no reservation")]
    Untracked,

    #[error("{} is not a chain this build serves", chain.display_name())]
    ForeignChain { chain: ChainId },
    #[error("{} does not belong to tron", asset.key())]
    ForeignAsset { asset: AssetId },
    #[error("that fee selection does not belong to tron")]
    ForeignFeeSelection,
    #[error("This address is no longer available in this wallet.")]
    NotWatched { address_id: i64 },
    #[error("Choose one address to send from.")]
    NotOneSource,
    /// The sending address left the watch set between build and signing, so
    /// there's no row to record the send against.
    #[error("The sending address is no longer available in this wallet.")]
    OwnerNotWatched,
    /// The detail comes from the address codec, whose messages never carry a
    /// fragment of their input.
    #[error("Enter a valid TRON address for {field}. {detail}")]
    InvalidAddress { field: &'static str, detail: String },

    #[error("TRON does not allow sending TRX to the address it comes from.")]
    TrxToSelf,
    #[error("Enter an amount above zero.")]
    AmountZero,
    #[error("The amount exceeds TRON's limit.")]
    AmountTooLarge,
    /// Typed amounts, not text, so the frontend formats them with the asset's own
    /// decimals.
    #[error(
        "{} needed and only {} available in {}",
        needed.format(asset.decimals()),
        available.format(asset.decimals()),
        asset.coin().symbol()
    )]
    InsufficientFunds { asset: AssetId, needed: Amount, available: Amount },

    #[error(
        "The fee limit of {} exceeds the network maximum of {}.",
        trx_text(*requested_sun),
        trx_text(*maximum_sun)
    )]
    FeeLimitAboveMaximum { requested_sun: u64, maximum_sun: u64 },
    #[error("The value for {what} exceeds TRON's limit.")]
    Overflow { what: &'static str },
    #[error("The transaction is {bytes} bytes. The maximum is {maximum_bytes} bytes.")]
    TransactionTooLarge { bytes: usize, maximum_bytes: usize },

    #[error("{what} could not be encoded")]
    Encode { what: &'static str },
    #[error("the unsigned transaction payload is not one this build produced: {detail}")]
    Payload { detail: &'static str },
    #[error("a tron transaction could not be decoded")]
    Protobuf(#[from] prost::DecodeError),
    /// A decoded transaction we didn't build: an unknown contract, a field we
    /// never set, calldata that isn't a TRC-20 transfer, or a signed form that
    /// isn't one signature over one `raw_data`.
    #[error("the transaction is not one this build produced: {detail}")]
    Foreign { detail: &'static str },
    /// The preview handed back for signing doesn't match what the payload's bytes
    /// do.
    #[error("the preview does not describe the transaction it was built with")]
    PreviewMismatch,

    #[error("tron signs with secp256k1, not {}", got.as_str())]
    WrongKeyKind { got: KeyKind },
    #[error("the signing key does not hold the address this transaction spends from")]
    KeyNotOwner,
    #[error("the transaction could not be signed")]
    Signing(#[from] KeyError),
}

impl TronError {
    pub(crate) fn invalid_address(field: &'static str, detail: impl Into<String>) -> Self {
        Self::InvalidAddress { field, detail: detail.into() }
    }
}

impl From<TronError> for ChainError {
    fn from(error: TronError) -> Self {
        match error {
            TronError::Net(inner) => inner.into(),
            TronError::Rejected(rejection) => Self::Rejected(rejection),
            TronError::Cursor(source) => Self::internal("tron cursor storage", source),
            TronError::InsufficientFunds { asset, needed, available } => {
                Self::InsufficientFunds { asset, needed, available }
            }

            error @ (TronError::Answer { .. }
            | TronError::Shape { .. }
            | TronError::Fault { .. }
            | TronError::CallFailed { .. }) => Self::rpc(error),

            error @ (TronError::ForeignChain { .. }
            | TronError::ForeignAsset { .. }
            | TronError::ForeignFeeSelection) => Self::unsupported(error),

            error @ TronError::InvalidAddress { .. } => Self::invalid_address(error),

            error @ (TronError::NotWatched { .. }
            | TronError::NotOneSource
            | TronError::OwnerNotWatched
            | TronError::TrxToSelf
            | TronError::AmountZero
            | TronError::AmountTooLarge
            | TronError::FeeLimitAboveMaximum { .. }
            | TronError::Overflow { .. }
            | TronError::TransactionTooLarge { .. }) => Self::invalid_input(error),

            error @ (TronError::Payload { .. }
            | TronError::Foreign { .. }
            | TronError::PreviewMismatch
            | TronError::WrongKeyKind { .. }
            | TronError::KeyNotOwner
            | TronError::Signing(_)) => Self::signing(error),

            error @ TronError::Protobuf(_) => Self::internal("tron transaction decode", error),
            error @ TronError::Encode { .. } => Self::internal("tron encoding", error),
            error @ TronError::Untracked => Self::internal("tron adapter", error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::endpoint;

    /// The engine and the frontend both branch on these classes.
    #[test]
    fn a_failure_reaches_the_engine_in_the_class_that_describes_it() {
        assert!(matches!(
            ChainError::from(TronError::OwnerNotWatched),
            ChainError::InvalidInput(..)
        ));
        let unreadable =
            TronError::Answer { endpoint: endpoint(), route: Route::Account, line: 1, column: 1 };
        assert!(matches!(ChainError::from(unreadable), ChainError::Rpc(_)));
        assert!(matches!(
            ChainError::from(TronError::ForeignFeeSelection),
            ChainError::Unsupported(_)
        ));
        assert!(matches!(ChainError::from(TronError::KeyNotOwner), ChainError::Signing(_)));
        assert!(matches!(
            ChainError::from(TronError::Untracked),
            ChainError::Internal { context: "tron adapter", .. }
        ));
        assert!(matches!(
            ChainError::from(TronError::invalid_address("recipient", "wrong length")),
            ChainError::InvalidAddress(_)
        ));
        // A transport failure stays a `jelly_net` error, so the engine can tell it
        // was the network.
        let net = TronError::Net(NetError::Status { endpoint: endpoint(), status: 500 });
        assert!(!matches!(ChainError::from(net), ChainError::Internal { .. }));
    }
}
