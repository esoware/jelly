use std::error::Error as StdError;
use std::fmt;

use crate::address::AddressError;
use crate::amount::Amount;
use crate::asset::AssetId;
use crate::chain::{ChainId, ScriptKind};
use crate::endpoint::EndpointAttempt;
use crate::random::EntropyError;
use crate::tx::PendingTxId;
use crate::wire_enum;

pub type BoxedSource = Box<dyn StdError + Send + Sync>;

/// Adapter failures safe to log with `Debug`. Use `EndpointLabel` for endpoints. Never
/// include secrets, raw URLs, or unredacted response bodies.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
    #[error("no {} endpoint answered for {operation}", chain.display_name())]
    EndpointsDown { chain: ChainId, operation: &'static str, attempts: Vec<EndpointAttempt> },
    #[error("{0}")]
    Rejected(Rejection),
    #[error("rpc: {0}")]
    Rpc(#[source] BoxedSource),
    #[error("invalid address: {0}")]
    InvalidAddress(#[source] BoxedSource),
    #[error("insufficient funds: need {needed}, have {available}")]
    InsufficientFunds { asset: AssetId, needed: Amount, available: Amount },
    #[error("signing: {0}")]
    Signing(#[source] BoxedSource),
    #[error("unsupported: {0}")]
    Unsupported(#[source] BoxedSource),
    /// The shell shows this text to the user verbatim, so it is copy: full sentences in
    /// sentence case, ASCII only, with no URL, file path, secret, or library error text.
    #[error("{0}")]
    InvalidInput(String, #[source] BoxedSource),
    #[error("{} speaks protocol v{version}, which is newer than this build", chain.display_name())]
    ProtocolTooNew { chain: ChainId, version: String },
    #[error("internal: {context}")]
    Internal {
        context: &'static str,
        #[source]
        source: Option<BoxedSource>,
    },
}

impl ChainError {
    pub fn rpc(source: impl Into<BoxedSource>) -> Self {
        Self::Rpc(source.into())
    }

    pub fn invalid_address(source: impl Into<BoxedSource>) -> Self {
        Self::InvalidAddress(source.into())
    }

    pub fn signing(source: impl Into<BoxedSource>) -> Self {
        Self::Signing(source.into())
    }

    pub fn unsupported(source: impl Into<BoxedSource>) -> Self {
        Self::Unsupported(source.into())
    }

    pub fn invalid_input(source: impl Into<BoxedSource>) -> Self {
        let source = source.into();
        Self::InvalidInput(source.to_string(), source)
    }

    pub fn protocol_too_new(chain: ChainId, version: impl Into<String>) -> Self {
        Self::ProtocolTooNew { chain, version: version.into() }
    }

    pub const fn endpoints_down(
        chain: ChainId,
        operation: &'static str,
        attempts: Vec<EndpointAttempt>,
    ) -> Self {
        Self::EndpointsDown { chain, operation, attempts }
    }

    pub const fn internal_without_source(context: &'static str) -> Self {
        Self::Internal { context, source: None }
    }

    pub fn internal<E: StdError + Send + Sync + 'static>(context: &'static str, source: E) -> Self {
        Self::Internal { context, source: Some(Box::new(source)) }
    }

    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::EndpointsDown { .. } => ErrorCode::EndpointsDown,
            Self::Rejected(_) => ErrorCode::Rejected,
            Self::Rpc(_) => ErrorCode::Rpc,
            Self::InvalidAddress(_) => ErrorCode::InvalidAddress,
            Self::InsufficientFunds { .. } => ErrorCode::InsufficientFunds,
            Self::Signing(_) => ErrorCode::Signing,
            Self::Unsupported(_) => ErrorCode::Unsupported,
            Self::InvalidInput(..) => ErrorCode::InvalidInput,
            Self::ProtocolTooNew { .. } => ErrorCode::ProtocolTooNew,
            Self::Internal { .. } => ErrorCode::Internal,
        }
    }
}

/// A rejection classified by the adapter. Other crates must not parse the node's message
/// again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rejection {
    pub kind: RejectionKind,
    pub retry: RetryPolicy,
    /// Redacted text for `Display`. Never empty, so use a fallback when the node gives no
    /// readable message.
    pub detail: String,
}

impl Rejection {
    pub fn new(kind: RejectionKind, retry: RetryPolicy, detail: impl Into<String>) -> Self {
        Self { kind, retry, detail: detail.into() }
    }
}

impl fmt::Display for Rejection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rejected ({}): {}", self.kind.as_str(), self.detail)
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Why the network refused a transaction.
    pub enum RejectionKind {
        AlreadyKnown => "already_known",
        BelowRelayFee => "below_relay_fee",
        BelowIncrementalRelayFee => "below_incremental_relay_fee",
        /// A required coin, output, or account is missing.
        MissingOrSpent => "missing_or_spent",
        InsufficientFunds => "insufficient_funds",
        DoubleSpend => "double_spend",
        Dust => "dust",
        TooLongMempoolChain => "too_long_mempool_chain",
        NonceTooLow => "nonce_too_low",
        NonceTooHigh => "nonce_too_high",
        ReplacementUnderpriced => "replacement_underpriced",
        GasTooLow => "gas_too_low",
        StaleReference => "stale_reference",
        Reverted => "reverted",
        Unclassified => "unclassified",
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// What the caller may do after a rejection.
    pub enum RetryPolicy {
        /// The transaction is in no mempool. Rebuild and resend.
        DidNotLand => "did_not_land",
        /// Every endpoint will refuse this transaction as it stands.
        FinalEverywhere => "final_everywhere",
        /// This endpoint refused, but another may accept.
        TryAnotherEndpoint => "try_another_endpoint",
        /// The network already has it, so the send is done.
        AlreadyLanded => "already_landed",
    }
}

/// Engine and command errors. `code` must classify every variant for the frontend.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("wallet is locked")]
    Locked,
    #[error("wrong password")]
    WrongPassword,
    #[error("wrong passphrase")]
    WrongPassphrase,
    #[error("this wallet needs its passphrase at every unlock")]
    PassphraseRequired,
    #[error("{entity} {id} does not exist")]
    NotFound { entity: &'static str, id: String },
    #[error("{entity} already exists")]
    AlreadyExists { entity: &'static str },
    /// The shell shows `detail` to the user verbatim, so it is copy: full sentences in
    /// sentence case, ASCII only, with no URL, file path, secret, or library error text.
    #[error("{detail}")]
    InvalidInput { field: &'static str, detail: String },
    #[error("no adapter for {}", .0.display_name())]
    NoAdapter(ChainId),
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error("this action needs the typed confirmation \"{expected}\"")]
    ConfirmationRequired { expected: String },
    #[error("no pending transaction {0}")]
    UnknownPendingTx(PendingTxId),
    #[error("the transaction is stale and must be rebuilt before it can be signed")]
    RebuildRequired,
    #[error("address {address_id} does not match the key it was derived from")]
    AddressMismatch { address_id: i64 },
    #[error(transparent)]
    Chain(#[from] ChainError),
    #[error("storage: {context}")]
    Storage {
        context: &'static str,
        #[source]
        source: BoxedSource,
    },
    #[error("keychain: {context}")]
    Keychain {
        context: &'static str,
        #[source]
        source: BoxedSource,
    },
    #[error("internal: {context}")]
    Internal {
        context: &'static str,
        #[source]
        source: Option<BoxedSource>,
    },
}

impl CoreError {
    pub fn not_found(entity: &'static str, id: impl Into<String>) -> Self {
        Self::NotFound { entity, id: id.into() }
    }

    pub fn invalid_input(field: &'static str, detail: impl Into<String>) -> Self {
        Self::InvalidInput { field, detail: detail.into() }
    }

    pub fn storage<E: StdError + Send + Sync + 'static>(context: &'static str, source: E) -> Self {
        Self::Storage { context, source: Box::new(source) }
    }

    pub fn keychain<E: StdError + Send + Sync + 'static>(context: &'static str, source: E) -> Self {
        Self::Keychain { context, source: Box::new(source) }
    }

    pub const fn internal_without_source(context: &'static str) -> Self {
        Self::Internal { context, source: None }
    }

    pub fn internal<E: StdError + Send + Sync + 'static>(context: &'static str, source: E) -> Self {
        Self::Internal { context, source: Some(Box::new(source)) }
    }

    pub const fn code(&self) -> ErrorCode {
        match self {
            Self::Locked => ErrorCode::Locked,
            Self::WrongPassword => ErrorCode::WrongPassword,
            Self::WrongPassphrase => ErrorCode::WrongPassphrase,
            Self::PassphraseRequired => ErrorCode::PassphraseRequired,
            Self::NotFound { .. } => ErrorCode::NotFound,
            Self::AlreadyExists { .. } => ErrorCode::AlreadyExists,
            Self::InvalidInput { .. } => ErrorCode::InvalidInput,
            Self::NoAdapter(_) => ErrorCode::NoAdapter,
            Self::Unsupported(_) => ErrorCode::Unsupported,
            Self::ConfirmationRequired { .. } => ErrorCode::ConfirmationRequired,
            Self::UnknownPendingTx(_) => ErrorCode::UnknownPendingTx,
            Self::RebuildRequired => ErrorCode::RebuildRequired,
            Self::AddressMismatch { .. } => ErrorCode::AddressMismatch,
            Self::Chain(error) => error.code(),
            Self::Storage { .. } => ErrorCode::Storage,
            Self::Keychain { .. } => ErrorCode::Keychain,
            Self::Internal { .. } => ErrorCode::Internal,
        }
    }
}

/// Key, phrase, path, and address failures across every chain's key handling.
///
/// No variant, and no `Display` reachable from one, interpolates any fragment of its
/// input. Messages carry only positions, counts, and kinds, so a seed word, a
/// passphrase, or a key byte cannot reach a log.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("The system could not generate secure random bytes.")]
    Entropy(#[from] EntropyError),

    #[error("Enter 12, 15, 18, 21 or 24 words. You entered {got}.")]
    InvalidWordCount { got: usize },
    #[error("Enter 24 TON seed words. You entered {got}.")]
    TonSeedWordCount { got: usize },
    #[error("Word {} is not in the word list.", index.saturating_add(1))]
    UnknownWord { index: usize },
    #[error("The seed phrase checksum is incorrect. Check the words and their order.")]
    PhraseChecksum,
    #[error("Entropy must be 16, 20, 24, 28 or 32 bytes. This has {got}.")]
    InvalidEntropyLength { got: usize },
    #[error("A seed of {got} bytes is not supported.")]
    InvalidSeedLength { got: usize },

    /// `component` counts from zero and leaves out the leading `m`.
    #[error("Derivation path component {} is invalid.", component.saturating_add(1))]
    InvalidPath { component: usize },
    #[error("The path has {got} components. Use at most {max}.")]
    PathTooDeep { got: usize, max: usize },
    /// `component` counts as in [`KeyError::InvalidPath`].
    #[error("Add an apostrophe to harden path component {}.", component.saturating_add(1))]
    PathNotHardened { component: usize },
    #[error("Child index {index} exceeds the BIP-32 limit.")]
    ChildIndexOutOfRange { index: u32 },
    #[error("BIP-32 key derivation failed.")]
    Derivation,

    #[error("A private key must be 32 bytes. This has {got}.")]
    InvalidKeyLength { got: usize },
    #[error("The private key is invalid for this curve.")]
    InvalidSecretKey,
    #[error("The public key is invalid for this curve.")]
    InvalidPublicKey,
    #[error("This key is not a {} key.", chain.display_name())]
    WrongKeyKind { chain: ChainId },

    #[error(transparent)]
    Address(#[from] AddressError),
    #[error("{} does not support this address type.", chain.display_name())]
    ScriptUnsupported { chain: ChainId, script: ScriptKind },
    #[error("{} addresses do not have a script type.", chain.display_name())]
    ScriptNotApplicable { chain: ChainId },
    #[error("{} addresses are not subaddresses.", chain.display_name())]
    NoSubaddresses { chain: ChainId },
    #[error("{} has no seed words of its own.", chain.display_name())]
    NoSeedWords { chain: ChainId },

    #[error("Jelly does not recognize this key format.")]
    UnknownFormat,
    #[error("This is a seed phrase, not a private key. Restore it as a wallet instead.")]
    SeedPhrase,
    #[error("Jelly does not support uncompressed private keys.")]
    UncompressedKey,
    #[error("Enter the password for this encrypted key.")]
    PasswordRequired,
    #[error("The password is incorrect.")]
    WrongPassword,
    /// `format` is the key format's display name, such as `Litecoin WIF key`.
    #[error("This {format} cannot be used on {}.", chain.display_name())]
    WrongChain { format: &'static str, chain: ChainId },

    #[error("The key file is not valid keystore JSON.")]
    KeystoreMalformed,
    #[error("Keystore version {version} is not supported.")]
    KeystoreVersion { version: u32 },
    #[error("Jelly does not support this key file's encryption method.")]
    KeystoreUnsupported,
    #[error("The key file's encryption settings are out of range.")]
    KeystoreParams,
    #[error("The address in the key file does not match its private key.")]
    KeystoreAddressMismatch,

    #[error("internal: {context}")]
    Internal { context: &'static str },
}

impl KeyError {
    /// An invariant the key code believes it upholds turned out not to hold.
    pub const fn internal(context: &'static str) -> Self {
        Self::Internal { context }
    }
}

impl From<EntropyError> for CoreError {
    fn from(error: EntropyError) -> Self {
        Self::internal("entropy", error)
    }
}

impl From<KeyError> for CoreError {
    fn from(error: KeyError) -> Self {
        match error {
            KeyError::WrongPassword => Self::WrongPassword,
            error @ (KeyError::Entropy(_) | KeyError::Internal { .. }) => {
                Self::internal("key handling", error)
            }
            error @ KeyError::Address(_) => Self::invalid_input("address", error.to_string()),
            error @ (KeyError::InvalidPath { .. }
            | KeyError::PathTooDeep { .. }
            | KeyError::PathNotHardened { .. }
            | KeyError::ChildIndexOutOfRange { .. }) => {
                Self::invalid_input("path", error.to_string())
            }
            error @ (KeyError::InvalidWordCount { .. }
            | KeyError::TonSeedWordCount { .. }
            | KeyError::UnknownWord { .. }
            | KeyError::PhraseChecksum) => Self::invalid_input("phrase", error.to_string()),
            error @ (KeyError::InvalidEntropyLength { .. }
            | KeyError::InvalidSeedLength { .. }
            | KeyError::Derivation
            | KeyError::InvalidKeyLength { .. }
            | KeyError::InvalidSecretKey
            | KeyError::InvalidPublicKey
            | KeyError::WrongKeyKind { .. }
            | KeyError::ScriptUnsupported { .. }
            | KeyError::ScriptNotApplicable { .. }
            | KeyError::NoSubaddresses { .. }
            | KeyError::NoSeedWords { .. }
            | KeyError::UnknownFormat
            | KeyError::SeedPhrase
            | KeyError::UncompressedKey
            | KeyError::PasswordRequired
            | KeyError::WrongChain { .. }
            | KeyError::KeystoreMalformed
            | KeyError::KeystoreVersion { .. }
            | KeyError::KeystoreUnsupported
            | KeyError::KeystoreParams
            | KeyError::KeystoreAddressMismatch) => Self::invalid_input("key", error.to_string()),
        }
    }
}

wire_enum! {
    #[cfg_attr(feature = "ipc", derive(specta::Type))]
    /// Domain error codes. The shell adds its own codes for startup, prices, and panics.
    pub enum ErrorCode {
        Locked => "locked",
        WrongPassword => "wrong_password",
        WrongPassphrase => "wrong_passphrase",
        PassphraseRequired => "passphrase_required",
        NotFound => "not_found",
        AlreadyExists => "already_exists",
        InvalidInput => "invalid_input",
        InvalidAddress => "invalid_address",
        InsufficientFunds => "insufficient_funds",
        EndpointsDown => "endpoints_down",
        /// The node returned an error response.
        Rpc => "rpc",
        NoAdapter => "no_adapter",
        Unsupported => "unsupported",
        ConfirmationRequired => "confirmation_required",
        UnknownPendingTx => "unknown_pending_tx",
        RebuildRequired => "rebuild_required",
        Rejected => "rejected",
        Signing => "signing",
        ProtocolTooNew => "protocol_too_new",
        AddressMismatch => "address_mismatch",
        Storage => "storage",
        Keychain => "keychain",
        Internal => "internal",
    }
}
