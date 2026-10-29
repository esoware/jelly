use jelly_core::{
    AddressError, Amount, AssetId, ChainError, ChainId, CoreError, EndpointLabel, FailureKind,
    KeyKind, Rejection, TokenId,
};
use jelly_net::{NetError, Outcome};
use solana_message::CompileError;

use crate::fee::{MAX_CHOSEN_PRIORITY_FEE_LAMPORTS, sol_text};
use crate::method::Method;

pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const INVALID_REQUEST: i64 = -32600;
const INVALID_PARAMS: i64 = -32602;
/// `sendTransaction` reports a failed preflight simulation with this code.
pub(crate) const PREFLIGHT_FAILED: i64 = -32002;
/// A transaction whose signature doesn't verify.
pub(crate) const SIGNATURE_VERIFY_FAILED: i64 = -32003;
const SIGNATURE_LENGTH_MISMATCH: i64 = -32013;
const UNSUPPORTED_TRANSACTION_VERSION: i64 = -32015;

/// A node's own wording appears only in a classified [`Rejection`] and in
/// [`SolanaError::Rpc`].
///
/// Endpoints are always an [`EndpointLabel`], and no variant interpolates key
/// material, a URL or a query string, so `Debug` output is safe to log.
#[derive(Debug, thiserror::Error)]
pub enum SolanaError {
    #[error(transparent)]
    Net(NetError),
    /// The cluster refused a broadcast.
    #[error("{0}")]
    Rejected(Rejection),
    /// A JSON-RPC error with no more specific variant.
    #[error("{endpoint}: {method} failed with rpc error {code}: {message}")]
    Rpc { endpoint: EndpointLabel, method: Method, code: i64, message: String },
    /// An answer this build can't read, reported by position because serde's message
    /// quotes the answer back.
    #[error("{endpoint}: the {method} answer is unreadable at line {line} column {column}")]
    Answer { endpoint: EndpointLabel, method: Method, line: usize, column: usize },
    #[error("{endpoint}: the {method} answer carries neither a result nor an error")]
    EmptyAnswer { endpoint: EndpointLabel, method: Method },
    /// A wrong answer count makes every position in the batch meaningless.
    #[error("{endpoint}: a batch of {asked} {method} calls answered with {answered}")]
    BatchLength { endpoint: EndpointLabel, method: Method, asked: usize, answered: usize },
    /// Mapped explicitly rather than with `#[from]`, so no other [`CoreError`] is
    /// reported as a storage failure.
    #[error("the cursor store failed")]
    Cursor(#[source] CoreError),

    #[error("{} is not a chain this build serves", chain.display_name())]
    ForeignChain { chain: ChainId },
    #[error("{} does not belong to solana", asset.key())]
    ForeignAsset { asset: AssetId },
    #[error("that fee selection does not belong to solana")]
    ForeignFeeSelection,
    #[error("This address is no longer available in this wallet.")]
    NotWatched { address_id: i64 },
    #[error("Choose one address to send from.")]
    NotOneSource,
    /// The payer left the watch set between build and signature, so there is no
    /// row to record the send against.
    #[error("The sending address is no longer available in this wallet.")]
    PayerNotWatched,
    /// The reason is the address codec's, whose messages never carry a fragment
    /// of their input.
    #[error("Enter a valid Solana address for {field}. {reason}")]
    InvalidAddress { field: &'static str, reason: AddressError },

    #[error("Enter an amount above zero.")]
    AmountZero,
    #[error("The amount exceeds Solana's limit.")]
    AmountTooLarge,
    /// Carried as typed amounts rather than as text, so the interface formats
    /// them with the asset's own decimals.
    #[error(
        "{} needed and only {} available in {}",
        needed.format(asset.decimals()),
        available.format(asset.decimals()),
        asset.coin().symbol()
    )]
    InsufficientFunds { asset: AssetId, needed: Amount, available: Amount },
    #[error("This token has transfer settings that Jelly does not support.")]
    TokenConfiguration,
    /// Something other than a token account of the mint sits at one of its
    /// associated addresses, which the associated-token program never makes.
    #[error("the account at a {} token address is not a {} token account", token.coin().symbol(), token.coin().symbol())]
    UnreadableTokenAccount { token: TokenId },
    #[error(
        "Your {} token account is frozen, so it cannot send. Only the token's issuer can unfreeze it.",
        token.coin().symbol()
    )]
    FrozenSource { token: TokenId },
    #[error(
        "The recipient's {} token account is frozen, so it cannot receive tokens.",
        token.coin().symbol()
    )]
    FrozenDestination { token: TokenId },
    #[error(
        "The recipient's {} token account only accepts transfers that carry a memo, and Jelly can't attach a memo.",
        token.coin().symbol()
    )]
    MemoRequired { token: TokenId },
    /// The recipient is a token account of another mint or another program, so this
    /// token can't land there.
    #[error("This address is a token account for a different token, not {}.", token.coin().symbol())]
    ForeignMint { token: TokenId },
    /// Tokens sent to an address with no private key land in an account only the
    /// program behind it could move, which most programs never do.
    #[error(
        "This address belongs to a program, not a wallet. Paste the recipient's {} token account address instead.",
        token.coin().symbol()
    )]
    OffCurveRecipient { token: TokenId },
    /// A token program owns the recipient, but it isn't a token account. It could be a
    /// mint, for example.
    #[error("This address is a token record such as a mint, not a wallet or a token account.")]
    NotARecipient,
    /// The account at the recipient's associated token address was reassigned,
    /// so a transfer into it would credit whoever holds it now.
    #[error(
        "The recipient's {} token account belongs to another address. Tokens sent to it would not reach the recipient.",
        token.coin().symbol()
    )]
    ForeignTokenAccount { token: TokenId },

    #[error("Enter a whole number for {field}.")]
    NotAnInteger { field: &'static str },
    #[error("The value for {what} exceeds Solana's limit.")]
    Overflow { what: &'static str },
    /// Guards the type, not an answer. No compiled limit is zero, and a simulation that
    /// reports zero units is never taken as a measurement.
    #[error("a compute unit limit of zero cannot run a transaction")]
    ComputeUnitsZero,
    #[error("The priority fee can be at most {}.", sol_text(MAX_CHOSEN_PRIORITY_FEE_LAMPORTS))]
    PriorityFeeAboveMaximum,
    #[error("The signed transaction is {bytes} bytes. The maximum is {maximum_bytes} bytes.")]
    TransactionTooLarge { bytes: usize, maximum_bytes: usize },
    #[error("the transaction message could not be compiled")]
    Compile(#[source] CompileError),
    #[error("the {what} could not be encoded")]
    Encode { what: &'static str },
    #[error("an empty batch was assembled")]
    EmptyBatch,

    #[error("the unsigned transaction payload is not a solana transaction: {detail}")]
    Payload { detail: String },

    #[error("solana signing needs an ed25519 seed, not {}", got.as_str())]
    WrongKeyKind { got: KeyKind },
    #[error("an ed25519 seed is thirty-two bytes; this is {got}")]
    WrongKeyLength { got: usize },
    #[error("the signing key does not own the account this transaction pays from")]
    KeyNotOwner,
    /// This adapter builds single-signer transfers, so a message wanting any other
    /// count wasn't built here and mustn't be signed here.
    #[error("this build signs exactly one signature and this message requires {required}")]
    SignerCount { required: u8 },
    /// Bytes this adapter isn't following, either never signed here or no longer
    /// followed. They aren't sent, since no record of the send could be written first.
    #[error("the signed transaction has no reservation")]
    Untracked,
    /// The node echoed a signature other than the one in the bytes it was given, so
    /// its answer isn't about this transaction.
    #[error("{endpoint}: the node echoed a signature that is not the one it was sent")]
    SignatureEcho { endpoint: EndpointLabel },
}

impl SolanaError {
    pub(crate) fn unreadable(
        endpoint: &EndpointLabel,
        method: Method,
        source: &serde_json::Error,
    ) -> Self {
        Self::Answer {
            endpoint: endpoint.clone(),
            method,
            line: source.line(),
            column: source.column(),
        }
    }

    pub(crate) fn payload(detail: impl Into<String>) -> Self {
        Self::Payload { detail: detail.into() }
    }

    /// Public so a scripted [`crate::SolanaNode`] classifies failures exactly as
    /// the shipped one does.
    pub fn from_net(method: Method, error: NetError) -> Self {
        match error {
            NetError::Rpc { endpoint, code, message } => {
                Self::Rpc { endpoint, method, code, message }
            }
            other @ (NetError::Endpoint { .. }
            | NetError::Timeout { .. }
            | NetError::Protocol { .. }
            | NetError::TooLarge { .. }
            | NetError::Status { .. }
            | NetError::RateLimited { .. }
            | NetError::WrongNetwork { .. }
            | NetError::NoEndpoints { .. }
            | NetError::NoneReady { .. }
            | NetError::AllFailed { .. }
            | NetError::InvalidUrl { .. }
            | NetError::Defaults { .. }
            | NetError::Config { .. }) => match other.into_source_of::<Self>() {
                Ok(inner) => inner,
                Err(other) => Self::Net(other),
            },
        }
    }

    const fn label(&self) -> Option<&EndpointLabel> {
        match *self {
            Self::Net(ref inner) => inner.label(),
            Self::Rpc { ref endpoint, .. }
            | Self::Answer { ref endpoint, .. }
            | Self::EmptyAnswer { ref endpoint, .. }
            | Self::BatchLength { ref endpoint, .. }
            | Self::SignatureEcho { ref endpoint } => Some(endpoint),
            Self::Rejected(_)
            | Self::Cursor(_)
            | Self::ForeignChain { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::PayerNotWatched
            | Self::InvalidAddress { .. }
            | Self::AmountZero
            | Self::AmountTooLarge
            | Self::InsufficientFunds { .. }
            | Self::NotAnInteger { .. }
            | Self::Overflow { .. }
            | Self::ComputeUnitsZero
            | Self::PriorityFeeAboveMaximum
            | Self::TransactionTooLarge { .. }
            | Self::Compile(_)
            | Self::Encode { .. }
            | Self::EmptyBatch
            | Self::Payload { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::SignerCount { .. }
            | Self::Untracked
            | Self::TokenConfiguration
            | Self::UnreadableTokenAccount { .. }
            | Self::FrozenSource { .. }
            | Self::FrozenDestination { .. }
            | Self::MemoRequired { .. }
            | Self::ForeignMint { .. }
            | Self::OffCurveRecipient { .. }
            | Self::NotARecipient
            | Self::ForeignTokenAccount { .. } => None,
        }
    }

    const fn kind(&self) -> FailureKind {
        match *self {
            Self::Net(ref inner) => inner.kind(),
            Self::Rpc { .. }
            | Self::Answer { .. }
            | Self::EmptyAnswer { .. }
            | Self::BatchLength { .. }
            | Self::SignatureEcho { .. }
            | Self::Rejected(_) => FailureKind::Protocol,
            Self::Cursor(_)
            | Self::ForeignChain { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::PayerNotWatched
            | Self::InvalidAddress { .. }
            | Self::AmountZero
            | Self::AmountTooLarge
            | Self::InsufficientFunds { .. }
            | Self::NotAnInteger { .. }
            | Self::Overflow { .. }
            | Self::ComputeUnitsZero
            | Self::PriorityFeeAboveMaximum
            | Self::TransactionTooLarge { .. }
            | Self::Compile(_)
            | Self::Encode { .. }
            | Self::EmptyBatch
            | Self::Payload { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::SignerCount { .. }
            | Self::Untracked
            | Self::TokenConfiguration
            | Self::UnreadableTokenAccount { .. }
            | Self::FrozenSource { .. }
            | Self::FrozenDestination { .. }
            | Self::MemoRequired { .. }
            | Self::ForeignMint { .. }
            | Self::OffCurveRecipient { .. }
            | Self::NotARecipient
            | Self::ForeignTokenAccount { .. } => FailureKind::Client,
        }
    }

    pub(crate) const fn outcome(&self) -> Outcome {
        match *self {
            Self::Net(_)
            | Self::Answer { .. }
            | Self::EmptyAnswer { .. }
            | Self::BatchLength { .. } => Outcome::Rotate,
            Self::Rpc { code, .. } => code_outcome(code),
            // These are about the request itself, so every endpoint would answer the
            // same way.
            Self::Rejected(_)
            | Self::Cursor(_)
            | Self::ForeignChain { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::PayerNotWatched
            | Self::InvalidAddress { .. }
            | Self::AmountZero
            | Self::AmountTooLarge
            | Self::InsufficientFunds { .. }
            | Self::NotAnInteger { .. }
            | Self::Overflow { .. }
            | Self::ComputeUnitsZero
            | Self::PriorityFeeAboveMaximum
            | Self::TransactionTooLarge { .. }
            | Self::Compile(_)
            | Self::Encode { .. }
            | Self::EmptyBatch
            | Self::Payload { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::SignerCount { .. }
            | Self::Untracked
            | Self::SignatureEcho { .. }
            | Self::TokenConfiguration
            | Self::UnreadableTokenAccount { .. }
            | Self::FrozenSource { .. }
            | Self::FrozenDestination { .. }
            | Self::MemoRequired { .. }
            | Self::ForeignMint { .. }
            | Self::OffCurveRecipient { .. }
            | Self::NotARecipient
            | Self::ForeignTokenAccount { .. } => Outcome::Fail,
        }
    }
}

/// A malformed request fails without rotating, because another endpoint would reject
/// it too. Method-not-found and Solana's availability errors rotate.
const fn code_outcome(code: i64) -> Outcome {
    match code {
        PARSE_ERROR
        | INVALID_REQUEST
        | INVALID_PARAMS
        | SIGNATURE_VERIFY_FAILED
        | SIGNATURE_LENGTH_MISMATCH
        | UNSUPPORTED_TRANSACTION_VERSION => Outcome::Fail,
        _ => Outcome::Rotate,
    }
}

/// Classifies pool failures for every call, broadcasts included. A broadcast refusal
/// never gets here, because [`crate::BroadcastReply::read`] returns it as a value.
pub(crate) fn classify(error: &NetError) -> Outcome {
    match *error {
        NetError::Rpc { code, .. } => code_outcome(code),
        NetError::Endpoint { .. }
        | NetError::Timeout { .. }
        | NetError::Protocol { .. }
        | NetError::TooLarge { .. }
        | NetError::Status { .. }
        | NetError::RateLimited { .. }
        | NetError::WrongNetwork { .. }
        | NetError::NoEndpoints { .. }
        | NetError::NoneReady { .. }
        | NetError::AllFailed { .. }
        | NetError::InvalidUrl { .. }
        | NetError::Defaults { .. }
        | NetError::Config { .. } => {
            error.source_of::<SolanaError>().map_or(Outcome::Rotate, SolanaError::outcome)
        }
    }
}

impl From<SolanaError> for ChainError {
    fn from(error: SolanaError) -> Self {
        match error {
            SolanaError::Net(inner) => inner.into(),
            SolanaError::Rejected(rejection) => Self::Rejected(rejection),
            SolanaError::Cursor(source) => Self::internal("solana cursor storage", source),
            SolanaError::InsufficientFunds { asset, needed, available } => {
                Self::InsufficientFunds { asset, needed, available }
            }

            error @ (SolanaError::Rpc { .. }
            | SolanaError::Answer { .. }
            | SolanaError::EmptyAnswer { .. }
            | SolanaError::BatchLength { .. }
            | SolanaError::SignatureEcho { .. }
            | SolanaError::UnreadableTokenAccount { .. }) => Self::rpc(error),

            error @ (SolanaError::ForeignChain { .. }
            | SolanaError::ForeignAsset { .. }
            | SolanaError::ForeignFeeSelection
            | SolanaError::TokenConfiguration) => Self::unsupported(error),

            error @ SolanaError::InvalidAddress { .. } => Self::invalid_address(error),

            error @ (SolanaError::NotWatched { .. }
            | SolanaError::NotOneSource
            | SolanaError::PayerNotWatched
            | SolanaError::AmountZero
            | SolanaError::AmountTooLarge
            | SolanaError::NotAnInteger { .. }
            | SolanaError::Overflow { .. }
            | SolanaError::PriorityFeeAboveMaximum
            | SolanaError::TransactionTooLarge { .. }
            | SolanaError::FrozenSource { .. }
            | SolanaError::FrozenDestination { .. }
            | SolanaError::MemoRequired { .. }
            | SolanaError::ForeignMint { .. }
            | SolanaError::OffCurveRecipient { .. }
            | SolanaError::NotARecipient
            | SolanaError::ForeignTokenAccount { .. }) => Self::invalid_input(error),

            error @ (SolanaError::Payload { .. }
            | SolanaError::WrongKeyKind { .. }
            | SolanaError::WrongKeyLength { .. }
            | SolanaError::KeyNotOwner
            | SolanaError::SignerCount { .. }) => Self::signing(error),

            error @ (SolanaError::ComputeUnitsZero
            | SolanaError::Compile(_)
            | SolanaError::Encode { .. }
            | SolanaError::EmptyBatch
            | SolanaError::Untracked) => Self::internal("solana adapter", error),
        }
    }
}

/// Keeps the original as the source, so the typed value survives the round trip
/// through the pool.
impl From<SolanaError> for NetError {
    fn from(error: SolanaError) -> Self {
        if let SolanaError::Net(net) = error {
            return net;
        }
        let Some(endpoint) = error.label().cloned() else {
            return Self::config("a solana failure with no endpoint", error);
        };
        let kind = error.kind();
        Self::endpoint(&endpoint, kind, error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::endpoint;

    fn rpc(code: i64, message: &str) -> NetError {
        NetError::Rpc { endpoint: endpoint(), code, message: message.to_owned() }
    }

    fn unreadable(method: Method, line: usize, column: usize) -> SolanaError {
        SolanaError::Answer { endpoint: endpoint(), method, line, column }
    }

    #[test]
    fn an_availability_code_rotates_and_a_malformed_request_stops_the_walk() {
        for code in [-32700, -32600, -32602, -32003, -32013, -32015] {
            assert_eq!(classify(&rpc(code, "whatever")), Outcome::Fail, "{code}");
        }
        for code in [-32601, -32005, -32004, -32010, -32011, -32603, 429] {
            assert_eq!(classify(&rpc(code, "whatever")), Outcome::Rotate, "{code}");
        }
        assert_eq!(classify(&NetError::NoEndpoints { chain: ChainId::Solana }), Outcome::Rotate);
    }

    /// The engine and the interface both branch on these classes. The conversion
    /// has no wildcard arm, so this pins what each class is for.
    #[test]
    fn a_failure_reaches_the_engine_in_the_class_that_describes_it() {
        assert!(matches!(
            ChainError::from(SolanaError::PayerNotWatched),
            ChainError::InvalidInput(..)
        ));
        assert!(matches!(ChainError::from(unreadable(Method::GetSlot, 1, 1)), ChainError::Rpc(_)));
        assert!(matches!(
            ChainError::from(SolanaError::ForeignFeeSelection),
            ChainError::Unsupported(_)
        ));
        assert!(matches!(ChainError::from(SolanaError::KeyNotOwner), ChainError::Signing(_)));
        assert!(matches!(
            ChainError::from(SolanaError::SignerCount { required: 2 }),
            ChainError::Signing(_)
        ));
        assert!(matches!(
            ChainError::from(SolanaError::InvalidAddress {
                field: "recipient",
                reason: AddressError::WrongLength { chain: ChainId::Solana },
            }),
            ChainError::InvalidAddress(_)
        ));
        assert!(matches!(
            ChainError::from(SolanaError::Encode { what: "scan cursor" }),
            ChainError::Internal { .. }
        ));
        assert!(matches!(ChainError::from(SolanaError::EmptyBatch), ChainError::Internal { .. }));
        assert!(matches!(
            ChainError::from(SolanaError::Untracked),
            ChainError::Internal { context: "solana adapter", .. }
        ));
        // A transport failure keeps `jelly_net`'s own error rather than becoming an
        // internal one, so the engine can still see it was the network.
        let net = SolanaError::Net(NetError::Status { endpoint: endpoint(), status: 500 });
        assert!(!matches!(ChainError::from(net), ChainError::Internal { .. }));
    }

    #[test]
    fn a_typed_failure_survives_a_round_trip_through_the_pool() {
        let net = NetError::from(unreadable(Method::GetBalance, 1, 9));
        let recovered = SolanaError::from_net(Method::GetBalance, net);
        assert!(
            matches!(
                recovered,
                SolanaError::Answer { method: Method::GetBalance, line: 1, column: 9, .. }
            ),
            "{recovered:?}"
        );
    }

    #[test]
    fn a_failure_with_no_endpoint_still_survives_the_round_trip() {
        let net = NetError::from(SolanaError::AmountZero);
        assert!(matches!(net, NetError::Config { .. }));
        assert!(matches!(SolanaError::from_net(Method::GetSlot, net), SolanaError::AmountZero));
    }

    #[test]
    fn an_unreadable_answer_rotates_and_a_request_this_build_will_not_make_does_not() {
        assert_eq!(unreadable(Method::GetSlot, 1, 1).outcome(), Outcome::Rotate);
        assert_eq!(SolanaError::AmountZero.outcome(), Outcome::Fail);
    }
}
