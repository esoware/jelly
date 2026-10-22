use alloy::primitives::Address;
use alloy::sol_types;
use jelly_core::{
    Amount, AssetId, ChainError, ChainId, CoreError, EndpointLabel, FailureKind, KeyError, KeyKind,
    Rejection,
};
use jelly_net::{NetError, Outcome, json_rpc_outcome};

use crate::broadcast::BroadcastRefusal;
use crate::method::Method;

/// A node's own wording appears only in a classified [`Rejection`] and in
/// [`EvmError::Rpc`] and [`EvmError::Reverted`].
///
/// Endpoints are always an [`EndpointLabel`], and no variant interpolates key
/// material, a URL or a query string, so `Debug` output is safe to log.
#[derive(Debug, thiserror::Error)]
pub enum EvmError {
    #[error(transparent)]
    Net(NetError),
    /// The node refused a broadcast, classified once.
    #[error("{0}")]
    Rejected(Rejection),
    /// A JSON-RPC error with no more specific variant.
    #[error("{endpoint}: {method} failed with rpc error {code}: {message}")]
    Rpc { endpoint: EndpointLabel, method: Method, code: i64, message: String },
    /// The call reverted on chain, which is the same answer everywhere.
    #[error("{endpoint}: {method} reverted: {message}")]
    Reverted { endpoint: EndpointLabel, method: Method, message: String },
    /// Not a host failure. The window shrinks and the walk retries.
    #[error("{endpoint}: the log window is wider than this endpoint allows")]
    LogWindowTooWide { endpoint: EndpointLabel },
    /// An answer this build cannot read, attributed to the endpoint so the walk
    /// rotates.
    #[error("{endpoint}: the {method} answer cannot be used: {detail}")]
    Answer { endpoint: EndpointLabel, method: Method, detail: String },
    /// Mapped explicitly rather than with `#[from]`, so no other [`CoreError`] is
    /// reported as a storage failure.
    #[error("the cursor store failed")]
    Cursor(#[source] CoreError),
    #[error("stored EVM state is unreadable")]
    UnreadableState,
    #[error("the signing account has not been restored")]
    UnwatchedSigner,

    #[error("{} is not the network this adapter serves", chain.display_name())]
    ForeignChain { chain: ChainId },
    #[error("chain id {chain_id} is not the network this adapter serves")]
    ForeignChainId { chain_id: u64 },
    #[error("{} does not belong to {}", asset.key(), chain.display_name())]
    ForeignAsset { asset: AssetId, chain: ChainId },
    #[error("that fee selection does not belong to an evm network")]
    ForeignFeeSelection,
    #[error("This address is no longer available in this wallet.")]
    NotWatched { address_id: i64 },
    #[error("Choose one address to send from.")]
    NotOneSource,
    /// The detail comes from the address codec, whose messages never carry a
    /// fragment of their input.
    #[error("Enter a valid EVM address. {detail}")]
    InvalidAddress { detail: String },
    #[error("Enter a 64-digit hexadecimal transaction ID.")]
    NotATxid,
    /// The adapter is not following this transaction, so there is no reserved
    /// nonce slot to send or replace it with.
    #[error("Transaction {txid} is not pending in this wallet.")]
    NotOurs { txid: String },
    /// A replacement of a transaction already seen in a block. Its nonce is
    /// spent, so the network would refuse it.
    #[error("Transaction {txid} is already confirmed and cannot be replaced.")]
    AlreadyConfirmed { txid: String },
    /// Carried as typed amounts rather than as text, so the interface formats
    /// them with the asset's own decimals.
    #[error(
        "{} needed and only {} available in {}",
        needed.format(asset.decimals()),
        available.format(asset.decimals()),
        asset.symbol()
    )]
    InsufficientFunds { asset: AssetId, needed: Amount, available: Amount },

    #[error("Enter a whole decimal or 0x-prefixed hexadecimal number for {field}.")]
    NotAnInteger { field: &'static str },
    #[error("Enter hexadecimal data for {field}.")]
    NotHex { field: &'static str },
    #[error("The value for {field} exceeds the {bits}-bit limit.")]
    IntegerTooWide { field: &'static str, bits: u32 },
    #[error("the {what} does not fit in {bits} bits")]
    ValueTooWide { what: &'static str, bits: u32 },
    #[error("The gas limit of {gas_limit} exceeds the network maximum of {cap_gas}.")]
    GasAboveCap { gas_limit: u64, cap_gas: u64 },
    #[error(
        "The priority fee of {tip_wei_per_gas} wei per gas exceeds the maximum fee of {cap_wei_per_gas} wei per gas."
    )]
    TipAboveCap { tip_wei_per_gas: u128, cap_wei_per_gas: u128 },
    #[error("The value for {field} is too large to calculate the fee.")]
    FeeOverflow { field: &'static str },
    #[error(
        "To replace nonce {nonce}, set at least {min_max_fee_per_gas} wei per gas for the maximum fee and {min_priority_fee_per_gas} wei per gas for the priority fee."
    )]
    ReplacementUnderpriced { nonce: u64, min_max_fee_per_gas: u128, min_priority_fee_per_gas: u128 },
    /// The gas estimate reverted, so the transaction would fail on chain and still
    /// pay for the gas it burned.
    #[error(
        "The network expects this transaction to fail, so Jelly will not send it. Check the recipient, the amount, and any custom data."
    )]
    WouldFail,

    #[error("there is no contract at {address} on {}", chain.display_name())]
    NoContract { address: Address, chain: ChainId },
    #[error("the {what} answer does not decode")]
    AbiDecode {
        what: &'static str,
        #[source]
        source: sol_types::Error,
    },
    #[error("the compiled-in {what} address is not an address")]
    BadConstant { what: &'static str },

    #[error("the unsigned transaction payload is not readable")]
    Payload {
        #[source]
        source: serde_json::Error,
    },
    #[error("the {what} could not be serialised")]
    Encode {
        what: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("an empty batch was assembled")]
    EmptyBatch,

    #[error("the history api has no key")]
    HistoryKeyMissing,
    #[error("the history api refused the request: {0}")]
    HistoryRefused(HistoryRefusal),
    #[error(
        "the history api answered in a shape this build cannot read at line {line} column {column}"
    )]
    HistoryShape { line: usize, column: usize },

    #[error("evm signing needs a secp256k1 key, not {got}")]
    WrongKeyKind { got: KeyKind },
    #[error("a secp256k1 scalar is thirty-two bytes; this is {got}")]
    WrongKeyLength { got: usize },
    #[error("the signing key does not own the sending address")]
    KeyNotOwner,
    #[error("the signature could not be produced")]
    Signing(#[source] KeyError),
    #[error("the signature carries recovery id {got}, which no typed transaction can encode")]
    UnencodableRecovery { got: u8 },
}

/// Why the history API would not answer.
///
/// A closed vocabulary rather than the service's text, so no third-party text is
/// kept and the user sees whether to fix the key or wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, thiserror::Error)]
pub enum HistoryRefusal {
    #[error("the api key was rejected")]
    InvalidKey,
    /// The key is good, but its plan does not include this network, which no
    /// amount of waiting changes.
    #[error("the api key's plan does not cover this network")]
    ChainNotCovered,
    #[error("the api key is over its rate limit")]
    RateLimited,
    #[error("the service refused the query")]
    BadQuery,
    #[error("the service reported a failure it did not explain")]
    Unclassified,
}

impl EvmError {
    pub(crate) fn answer(
        endpoint: &EndpointLabel,
        method: Method,
        detail: impl Into<String>,
    ) -> Self {
        Self::Answer { endpoint: endpoint.clone(), method, detail: detail.into() }
    }

    /// The method decides how a message is read, because the same wording can be
    /// a refusal for a broadcast and mean nothing for a log read.
    ///
    /// Public so a scripted [`crate::EvmNode`] classifies refusals the same way
    /// the shipped one does.
    pub fn from_net(method: Method, error: NetError) -> Self {
        match error {
            NetError::Rpc { endpoint, code, message } => {
                Self::from_rpc(method, endpoint, code, message)
            }
            other @ (NetError::Endpoint { .. }
            | NetError::Timeout { .. }
            | NetError::Protocol { .. }
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

    fn from_rpc(method: Method, endpoint: EndpointLabel, code: i64, message: String) -> Self {
        if method == Method::SendRawTransaction {
            let rejection = BroadcastRefusal::classify(&message);
            if rejection != BroadcastRefusal::Unclassified {
                return Self::Rejected(rejection.into_rejection(&message));
            }
        }
        if method == Method::GetLogs && is_range_complaint(&message) {
            return Self::LogWindowTooWide { endpoint };
        }
        if is_revert(&message) {
            return Self::Reverted { endpoint, method, message };
        }
        Self::Rpc { endpoint, method, code, message }
    }

    pub(crate) const fn label(&self) -> Option<&EndpointLabel> {
        match *self {
            Self::Net(ref inner) => inner.label(),
            Self::Rpc { ref endpoint, .. }
            | Self::Reverted { ref endpoint, .. }
            | Self::LogWindowTooWide { ref endpoint }
            | Self::Answer { ref endpoint, .. } => Some(endpoint),
            Self::Rejected(_)
            | Self::Cursor(_)
            | Self::UnreadableState
            | Self::UnwatchedSigner
            | Self::ForeignChain { .. }
            | Self::ForeignChainId { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::InvalidAddress { .. }
            | Self::NotATxid
            | Self::NotOurs { .. }
            | Self::AlreadyConfirmed { .. }
            | Self::InsufficientFunds { .. }
            | Self::NotAnInteger { .. }
            | Self::NotHex { .. }
            | Self::IntegerTooWide { .. }
            | Self::ValueTooWide { .. }
            | Self::GasAboveCap { .. }
            | Self::TipAboveCap { .. }
            | Self::FeeOverflow { .. }
            | Self::ReplacementUnderpriced { .. }
            | Self::WouldFail
            | Self::NoContract { .. }
            | Self::AbiDecode { .. }
            | Self::BadConstant { .. }
            | Self::Payload { .. }
            | Self::Encode { .. }
            | Self::EmptyBatch
            | Self::HistoryKeyMissing
            | Self::HistoryRefused(_)
            | Self::HistoryShape { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::Signing(_)
            | Self::UnencodableRecovery { .. } => None,
        }
    }

    pub(crate) const fn kind(&self) -> FailureKind {
        match *self {
            Self::Net(ref inner) => inner.kind(),
            Self::Rpc { .. }
            | Self::Reverted { .. }
            | Self::LogWindowTooWide { .. }
            | Self::Answer { .. }
            | Self::Rejected(_) => FailureKind::Protocol,
            Self::Cursor(_)
            | Self::UnreadableState
            | Self::UnwatchedSigner
            | Self::ForeignChain { .. }
            | Self::ForeignChainId { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::InvalidAddress { .. }
            | Self::NotATxid
            | Self::NotOurs { .. }
            | Self::AlreadyConfirmed { .. }
            | Self::InsufficientFunds { .. }
            | Self::NotAnInteger { .. }
            | Self::NotHex { .. }
            | Self::IntegerTooWide { .. }
            | Self::ValueTooWide { .. }
            | Self::GasAboveCap { .. }
            | Self::TipAboveCap { .. }
            | Self::FeeOverflow { .. }
            | Self::ReplacementUnderpriced { .. }
            | Self::WouldFail
            | Self::NoContract { .. }
            | Self::AbiDecode { .. }
            | Self::BadConstant { .. }
            | Self::Payload { .. }
            | Self::Encode { .. }
            | Self::EmptyBatch
            | Self::HistoryKeyMissing
            | Self::HistoryRefused(_)
            | Self::HistoryShape { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::Signing(_)
            | Self::UnencodableRecovery { .. } => FailureKind::Client,
        }
    }

    /// Transport failures, unreadable answers and non-final RPC errors rotate,
    /// because they say something about one host. Everything else stops the walk.
    pub(crate) const fn outcome(&self) -> Outcome {
        match *self {
            Self::Net(_) | Self::Answer { .. } => Outcome::Rotate,
            Self::Rpc { code, .. } => {
                if code == GETH_REVERT_CODE {
                    Outcome::Fail
                } else {
                    json_rpc_outcome(code)
                }
            }
            Self::Rejected(_)
            | Self::Reverted { .. }
            | Self::LogWindowTooWide { .. }
            | Self::Cursor(_)
            | Self::UnreadableState
            | Self::UnwatchedSigner
            | Self::ForeignChain { .. }
            | Self::ForeignChainId { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::InvalidAddress { .. }
            | Self::NotATxid
            | Self::NotOurs { .. }
            | Self::AlreadyConfirmed { .. }
            | Self::InsufficientFunds { .. }
            | Self::NotAnInteger { .. }
            | Self::NotHex { .. }
            | Self::IntegerTooWide { .. }
            | Self::ValueTooWide { .. }
            | Self::GasAboveCap { .. }
            | Self::TipAboveCap { .. }
            | Self::FeeOverflow { .. }
            | Self::ReplacementUnderpriced { .. }
            | Self::WouldFail
            | Self::NoContract { .. }
            | Self::AbiDecode { .. }
            | Self::BadConstant { .. }
            | Self::Payload { .. }
            | Self::Encode { .. }
            | Self::EmptyBatch
            | Self::HistoryKeyMissing
            | Self::HistoryRefused(_)
            | Self::HistoryShape { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::Signing(_)
            | Self::UnencodableRecovery { .. } => Outcome::Fail,
        }
    }

    pub(crate) const fn is_window_too_wide(&self) -> bool {
        matches!(*self, Self::LogWindowTooWide { .. })
    }

    /// A history failure that waiting cures, such as a busy, slow or unreachable
    /// service. A refused key or query and an unreadable answer are not.
    pub(crate) const fn is_transient(&self) -> bool {
        match *self {
            Self::Net(NetError::Status { status, .. }) => status >= 500,
            Self::Net(_) | Self::HistoryRefused(HistoryRefusal::RateLimited) => true,
            Self::HistoryRefused(
                HistoryRefusal::InvalidKey
                | HistoryRefusal::ChainNotCovered
                | HistoryRefusal::BadQuery
                | HistoryRefusal::Unclassified,
            )
            | Self::Rejected(_)
            | Self::Rpc { .. }
            | Self::Reverted { .. }
            | Self::LogWindowTooWide { .. }
            | Self::Answer { .. }
            | Self::Cursor(_)
            | Self::UnreadableState
            | Self::UnwatchedSigner
            | Self::ForeignChain { .. }
            | Self::ForeignChainId { .. }
            | Self::ForeignAsset { .. }
            | Self::ForeignFeeSelection
            | Self::NotWatched { .. }
            | Self::NotOneSource
            | Self::InvalidAddress { .. }
            | Self::NotATxid
            | Self::NotOurs { .. }
            | Self::AlreadyConfirmed { .. }
            | Self::InsufficientFunds { .. }
            | Self::NotAnInteger { .. }
            | Self::NotHex { .. }
            | Self::IntegerTooWide { .. }
            | Self::ValueTooWide { .. }
            | Self::GasAboveCap { .. }
            | Self::TipAboveCap { .. }
            | Self::FeeOverflow { .. }
            | Self::ReplacementUnderpriced { .. }
            | Self::WouldFail
            | Self::NoContract { .. }
            | Self::AbiDecode { .. }
            | Self::BadConstant { .. }
            | Self::Payload { .. }
            | Self::Encode { .. }
            | Self::EmptyBatch
            | Self::HistoryKeyMissing
            | Self::HistoryShape { .. }
            | Self::WrongKeyKind { .. }
            | Self::WrongKeyLength { .. }
            | Self::KeyNotOwner
            | Self::Signing(_)
            | Self::UnencodableRecovery { .. } => false,
        }
    }
}

impl From<EvmError> for ChainError {
    fn from(error: EvmError) -> Self {
        match error {
            EvmError::Net(inner) => inner.into(),
            EvmError::Rejected(rejection) => Self::Rejected(rejection),
            EvmError::Cursor(source) => Self::internal("evm cursor storage", source),
            error @ EvmError::UnreadableState => Self::internal("evm stored state", error),
            EvmError::InsufficientFunds { asset, needed, available } => {
                Self::InsufficientFunds { asset, needed, available }
            }

            error @ (EvmError::Rpc { .. }
            | EvmError::Reverted { .. }
            | EvmError::LogWindowTooWide { .. }
            | EvmError::Answer { .. }
            | EvmError::NoContract { .. }
            | EvmError::AbiDecode { .. }
            | EvmError::HistoryRefused(_)
            | EvmError::HistoryShape { .. }
            | EvmError::ValueTooWide { .. }) => Self::rpc(error),

            error @ (EvmError::ForeignChain { .. }
            | EvmError::ForeignChainId { .. }
            | EvmError::ForeignAsset { .. }
            | EvmError::ForeignFeeSelection
            | EvmError::HistoryKeyMissing) => Self::unsupported(error),

            error @ EvmError::InvalidAddress { .. } => Self::invalid_address(error),

            error @ (EvmError::NotWatched { .. }
            | EvmError::NotOneSource
            | EvmError::NotATxid
            | EvmError::NotOurs { .. }
            | EvmError::AlreadyConfirmed { .. }
            | EvmError::NotAnInteger { .. }
            | EvmError::NotHex { .. }
            | EvmError::IntegerTooWide { .. }
            | EvmError::GasAboveCap { .. }
            | EvmError::TipAboveCap { .. }
            | EvmError::FeeOverflow { .. }
            | EvmError::ReplacementUnderpriced { .. }
            | EvmError::WouldFail) => Self::invalid_input(error),

            error @ (EvmError::WrongKeyKind { .. }
            | EvmError::WrongKeyLength { .. }
            | EvmError::KeyNotOwner
            | EvmError::Signing(_)
            | EvmError::UnencodableRecovery { .. }) => Self::signing(error),

            error @ (EvmError::UnwatchedSigner
            | EvmError::Payload { .. }
            | EvmError::Encode { .. }
            | EvmError::EmptyBatch
            | EvmError::BadConstant { .. }) => Self::internal("evm adapter", error),
        }
    }
}

/// Keeps the original as the source, so the typed value survives the round trip
/// through the pool.
impl From<EvmError> for NetError {
    fn from(error: EvmError) -> Self {
        if let EvmError::Net(net) = error {
            return net;
        }
        let Some(endpoint) = error.label().cloned() else {
            return Self::config("an evm failure with no endpoint", error);
        };
        let kind = error.kind();
        Self::endpoint(&endpoint, kind, error)
    }
}

/// The classifier every idempotent read is walked with.
pub(crate) fn classify(error: &NetError) -> Outcome {
    match *error {
        NetError::Rpc { code, ref message, .. } => {
            if code == GETH_REVERT_CODE || is_revert(message) {
                Outcome::Fail
            } else {
                json_rpc_outcome(code)
            }
        }
        NetError::Endpoint { .. }
        | NetError::Timeout { .. }
        | NetError::Protocol { .. }
        | NetError::Status { .. }
        | NetError::RateLimited { .. }
        | NetError::WrongNetwork { .. }
        | NetError::NoEndpoints { .. }
        | NetError::NoneReady { .. }
        | NetError::AllFailed { .. }
        | NetError::InvalidUrl { .. }
        | NetError::Defaults { .. }
        | NetError::Config { .. } => {
            error.source_of::<EvmError>().map_or(Outcome::Rotate, EvmError::outcome)
        }
    }
}

/// The classifier a log read is walked with.
///
/// A window complaint stops the walk so the window shrinks at once, rather than
/// spending one request per endpoint on a width that is already too wide. A host
/// that does not serve this query is skipped without a penalty, because a backed
/// off host would also be lost to the balance reads that follow.
pub(crate) fn classify_logs(error: &NetError) -> Outcome {
    match *error {
        NetError::Rpc { ref message, .. } if is_range_complaint(message) => Outcome::Fail,
        NetError::Rpc { ref message, .. } if is_log_refusal(message) => Outcome::Skip,
        NetError::Status { status: 403, .. } => Outcome::Skip,
        NetError::Endpoint { .. }
        | NetError::Timeout { .. }
        | NetError::Protocol { .. }
        | NetError::Status { .. }
        | NetError::RateLimited { .. }
        | NetError::Rpc { .. }
        | NetError::WrongNetwork { .. }
        | NetError::NoEndpoints { .. }
        | NetError::NoneReady { .. }
        | NetError::AllFailed { .. }
        | NetError::InvalidUrl { .. }
        | NetError::Defaults { .. }
        | NetError::Config { .. } => classify(error),
    }
}

/// The classifier a broadcast is walked with.
///
/// A classified refusal counts as an answer, so the typed rejection reaches the
/// caller instead of being lost in an all-endpoints-failed report.
pub(crate) fn classify_broadcast(error: &NetError) -> Outcome {
    match *error {
        NetError::Rpc { ref message, .. } => {
            if BroadcastRefusal::classify(message) == BroadcastRefusal::Unclassified {
                Outcome::Rotate
            } else {
                Outcome::Accept
            }
        }
        NetError::Endpoint { .. }
        | NetError::Timeout { .. }
        | NetError::Protocol { .. }
        | NetError::Status { .. }
        | NetError::RateLimited { .. }
        | NetError::WrongNetwork { .. }
        | NetError::NoEndpoints { .. }
        | NetError::NoneReady { .. }
        | NetError::AllFailed { .. }
        | NetError::InvalidUrl { .. }
        | NetError::Defaults { .. }
        | NetError::Config { .. } => Outcome::Rotate,
    }
}

/// True when a JSON-RPC failure means the `eth_getLogs` window is too wide.
///
/// No provider reports this with a code, so the wording is the only signal. A
/// bare `limit exceeded` is not one, because the BNB Chain data seeds answer it
/// to every `eth_getLogs` whatever the width, and reading it as one pins the walk
/// to a host that will never serve a log.
fn is_range_complaint(message: &str) -> bool {
    const NEEDLES: [&str; 11] = [
        "block range",
        "range is too large",
        "ranges over",
        "query returned more than",
        "more than 10000 results",
        "response size exceed",
        "query timeout exceeded",
        "too many results",
        "exceeds max results",
        "up to a 1000 blocks range",
        "exceed maximum block range",
    ];
    let text = message.to_ascii_lowercase();
    !is_behind_head(&text) && NEEDLES.iter().any(|needle| text.contains(needle))
}

/// True when a host refuses `eth_getLogs` for a reason no window would fix.
///
/// The BNB Chain data seeds answer a bare `limit exceeded` to every log query, a
/// pruned node answers `header not found` below the history it keeps, and a node
/// behind the tip refuses a window that ends past its head.
fn is_log_refusal(message: &str) -> bool {
    let text = message.trim().to_ascii_lowercase();
    text == "limit exceeded" || text.contains("header not found") || is_behind_head(&text)
}

/// Gateways such as Pocket Network's sometimes route to a backend far behind the
/// tip, which names the block range in its refusal although the width is fine.
fn is_behind_head(lowercase: &str) -> bool {
    lowercase.contains("beyond current head")
}

/// True when a JSON-RPC failure means the call reverted on chain, which every
/// endpoint would repeat.
fn is_revert(message: &str) -> bool {
    let text = message.to_ascii_lowercase();
    text.contains("execution reverted") || text.contains("gas required exceeds allowance")
}

/// How geth and its forks report a reverted call, which every endpoint would repeat.
/// The standard JSON-RPC codes are `json_rpc_outcome`'s.
const GETH_REVERT_CODE: i64 = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_util::endpoint;

    fn rpc(code: i64, message: &str) -> NetError {
        NetError::Rpc { endpoint: endpoint(), code, message: message.to_owned() }
    }

    #[test]
    fn the_same_wording_means_different_things_to_different_methods() {
        let already = "already known";
        assert!(matches!(
            EvmError::from_net(Method::SendRawTransaction, rpc(-32000, already)),
            EvmError::Rejected(_)
        ));
        assert!(matches!(
            EvmError::from_net(Method::Call, rpc(-32000, already)),
            EvmError::Rpc { .. }
        ));

        let wide = "eth_getLogs block range is too large";
        assert!(matches!(
            EvmError::from_net(Method::GetLogs, rpc(-32000, wide)),
            EvmError::LogWindowTooWide { .. }
        ));
        assert!(matches!(
            EvmError::from_net(Method::Call, rpc(-32000, wide)),
            EvmError::Rpc { .. }
        ));
    }

    #[test]
    fn a_revert_is_named_whatever_method_produced_it() {
        for method in Method::ALL {
            let error = EvmError::from_net(method, rpc(3, "execution reverted"));
            if method == Method::SendRawTransaction {
                assert!(matches!(error, EvmError::Rejected(_)), "{method}");
                continue;
            }
            assert!(matches!(error, EvmError::Reverted { .. }), "{method}");
        }
    }

    #[test]
    fn an_unclassified_send_refusal_keeps_its_code_and_rotates() {
        let error =
            EvmError::from_net(Method::SendRawTransaction, rpc(-32099, "brand new wording"));
        assert!(matches!(error, EvmError::Rpc { code: -32099, .. }), "{error:?}");
        assert_eq!(classify_broadcast(&rpc(-32099, "brand new wording")), Outcome::Rotate);
        assert_eq!(classify_broadcast(&rpc(-32000, "nonce too low")), Outcome::Accept);
    }

    #[test]
    fn the_standard_codes_stop_a_read_and_a_transport_failure_rotates() {
        for code in [-32700, -32600, -32601, -32602, 3] {
            assert_eq!(classify(&rpc(code, "whatever")), Outcome::Fail, "{code}");
        }
        assert_eq!(classify(&rpc(-32603, "internal error")), Outcome::Rotate);
        assert_eq!(classify(&rpc(-32000, "execution reverted")), Outcome::Fail);
        assert_eq!(classify(&NetError::NoEndpoints { chain: ChainId::Ethereum }), Outcome::Rotate);
    }

    #[test]
    fn only_the_log_classifier_stops_the_walk_on_a_window_complaint() {
        let error = rpc(-32005, "query returned more than 10000 results");
        assert_eq!(classify(&error), Outcome::Rotate);
        assert_eq!(classify_logs(&error), Outcome::Fail);
    }

    /// The BNB Chain data seeds answer this to a one-block window too.
    ///
    /// The walk moves on to a host that serves logs instead of shrinking forever,
    /// and leaves the seed healthy for the balance reads it does serve.
    #[test]
    fn a_host_that_serves_no_logs_is_skipped_rather_than_shrinking_the_window() {
        let refusal = rpc(-32005, "limit exceeded");
        assert_eq!(classify_logs(&refusal), Outcome::Skip);
        let error = EvmError::from_net(Method::GetLogs, refusal);
        assert!(!error.is_window_too_wide(), "{error:?}");
    }

    /// Another host can serve the same window at full width, so the walk moves on
    /// rather than shrinking it for every host.
    #[test]
    fn a_host_behind_the_tip_is_skipped_rather_than_shrinking_the_window() {
        let behind = rpc(
            -32602,
            "block range extends beyond current head block: requested 52597358, head 52537347",
        );
        assert_eq!(classify_logs(&behind), Outcome::Skip);
        let error = EvmError::from_net(Method::GetLogs, behind);
        assert!(!error.is_window_too_wide(), "{error:?}");
    }

    /// A public node without archive access answers a log query below its recent
    /// window with http 403, and a pruned node with header not found. Neither says
    /// anything about a balance.
    #[test]
    fn a_host_without_the_history_a_log_query_needs_is_skipped() {
        let archive = NetError::Status { endpoint: endpoint(), status: 403 };
        assert_eq!(classify_logs(&archive), Outcome::Skip);
        assert_eq!(classify(&archive), Outcome::Rotate);
        assert_eq!(classify_logs(&rpc(-32000, "header not found")), Outcome::Skip);
        assert_eq!(classify_logs(&rpc(-32000, "rate limit exceeded")), Outcome::Rotate);
    }

    #[test]
    fn a_typed_failure_survives_a_round_trip_through_the_pool() {
        let original = EvmError::answer(&endpoint(), Method::GetLogs, "no topics");
        let net = NetError::from(original);
        let recovered = EvmError::from_net(Method::GetLogs, net);
        assert!(matches!(recovered, EvmError::Answer { method: Method::GetLogs, .. }));
    }

    #[test]
    fn a_failure_with_no_endpoint_still_survives_the_round_trip() {
        let original = EvmError::NotATxid;
        let net = NetError::from(original);
        assert!(matches!(net, NetError::Config { .. }));
        assert!(matches!(EvmError::from_net(Method::Call, net), EvmError::NotATxid));
    }

    #[test]
    fn a_log_window_complaint_is_never_read_as_a_send_refusal() {
        // A range complaint shares the words too large with a size refusal, so
        // the size row matches whole wordings.
        for message in [
            "eth_getLogs block range is too large, max 1000",
            "query returned more than 10000 results",
            "Log response size exceeded",
        ] {
            assert_eq!(
                BroadcastRefusal::classify(message),
                BroadcastRefusal::Unclassified,
                "{message}"
            );
            assert!(is_range_complaint(message), "{message}");
        }
    }

    #[test]
    fn a_range_complaint_is_recognised_in_every_wording_providers_use() {
        for message in [
            "eth_getLogs block range is too large, max 1000",
            "query returned more than 10000 results",
            "Log response size exceeded. You can make eth_getLogs requests with up to a 1000 blocks range",
            "Block range limit exceeded.",
        ] {
            assert!(is_range_complaint(message), "{message}");
        }
        for message in ["nonce too low", "execution reverted", "internal error"] {
            assert!(!is_range_complaint(message), "{message}");
        }
    }

    #[test]
    fn a_revert_is_read_from_its_wording_and_an_rpc_error_outcome_from_its_code() {
        assert!(is_revert("execution reverted: ERC20: insufficient allowance"));
        assert!(is_revert("gas required exceeds allowance (30000000)"));
        assert!(!is_revert("nonce too low"));
        let answer = |code| EvmError::Rpc {
            endpoint: endpoint(),
            method: Method::ChainId,
            code,
            message: String::new(),
        };
        for code in [-32700, -32600, -32601, -32602, 3] {
            assert_eq!(answer(code).outcome(), Outcome::Fail, "{code}");
        }
        for code in [-32000, -32005, -32010, -32603, 0] {
            assert_eq!(answer(code).outcome(), Outcome::Rotate, "{code}");
        }
    }
}
