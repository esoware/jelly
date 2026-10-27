use std::{fmt, io};

use jelly_core::{
    Amount, AssetId, ChainError, ChainId, CoreError, EndpointAttempt, ErrorCode, Rejection,
    RejectionKind, RetryPolicy, SkipReason, wire_enum,
};
use jelly_engine::StartupError;
use serde::Serialize;
use specta::Type;
use tokio::task::JoinError;

/// Process failures stay in Rust and never cross the IPC boundary.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("the async runtime could not be built")]
    Runtime {
        #[source]
        source: io::Error,
    },
    #[error("the application data directory is unavailable")]
    DataDirectory {
        #[source]
        source: tauri::Error,
    },
    #[error("the wallet window is missing from the configuration")]
    WindowConfig,
    #[error(transparent)]
    Tauri(#[from] tauri::Error),
}

wire_enum! {
    #[derive(Type)]
    pub enum ShellErrorCode {
        /// Startup timed out before the command ran.
        Starting => "starting",
        /// The price provider did not answer, or prices are off.
        PricesUnavailable => "prices_unavailable",
        /// The command task panicked.
        Panic => "panic",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Type)]
#[serde(untagged)]
pub enum IpcErrorCode {
    Domain(ErrorCode),
    Shell(ShellErrorCode),
}

impl From<ErrorCode> for IpcErrorCode {
    fn from(code: ErrorCode) -> Self {
        Self::Domain(code)
    }
}

impl fmt::Display for IpcErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Domain(code) => code.fmt(f),
            Self::Shell(code) => code.fmt(f),
        }
    }
}

/// Typed error details for the frontend. Amounts retain their base units so the frontend
/// can apply the asset's decimals.
///
/// A missing adapter has no reason when the chain is switched off or the wallet's
/// adapters are not built yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcErrorDetail {
    InsufficientFunds { asset: AssetId, needed: Amount, available: Amount },
    NoAdapter { chain: ChainId, reason: Option<SkipReason> },
    EndpointsDown { chain: ChainId, attempts: Vec<EndpointAttempt> },
    Rejected { rejection: RejectionKind, retry: RetryPolicy },
    ProtocolTooNew { chain: ChainId, version: String },
    SchemaTooNew { found: u32, supported: u32 },
}

/// Command failure returned to the frontend.
///
/// `message` is user-facing prose. It must not contain secrets, raw URLs, file paths, or
/// library error text. The frontend branches on `code` and `detail`, never on message
/// wording.
///
/// Errors are serialized only, never accepted as command arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type, thiserror::Error)]
#[error("{code}: {message}")]
#[serde(rename_all = "camelCase")]
pub struct IpcError {
    pub code: IpcErrorCode,
    pub message: String,
    pub detail: Option<IpcErrorDetail>,
}

impl IpcError {
    pub(crate) fn new(code: IpcErrorCode, message: &'static str) -> Self {
        Self { code, message: message.to_owned(), detail: None }
    }

    /// No command ran, so retrying is safe.
    pub fn starting() -> Self {
        Self::new(IpcErrorCode::Shell(ShellErrorCode::Starting), "Jelly is still starting.")
    }

    pub fn prices_unavailable() -> Self {
        Self::new(
            IpcErrorCode::Shell(ShellErrorCode::PricesUnavailable),
            "Prices are unavailable right now.",
        )
    }

    pub(crate) fn invalid_input(message: &'static str) -> Self {
        Self::new(ErrorCode::InvalidInput.into(), message)
    }

    /// Accepts a fixed user message. Log diagnostic details separately.
    pub(crate) fn internal(message: &'static str) -> Self {
        Self::new(ErrorCode::Internal.into(), message)
    }
}

/// Only invalid input carries its own text, which is copy by contract. Every other error
/// gets a safe fallback, so node text, contexts, and base units stay in Rust.
impl From<CoreError> for IpcError {
    fn from(error: CoreError) -> Self {
        let domain = error.code();
        let copy = "Jelly could not complete the request.";
        let (message, detail) = match error {
            CoreError::Chain(ChainError::InvalidInput(text, _)) => (text, None),
            CoreError::Chain(chain) => (copy.to_owned(), chain_detail(chain)),
            CoreError::NoAdapter { chain, reason } => {
                (copy.to_owned(), Some(IpcErrorDetail::NoAdapter { chain, reason }))
            }
            CoreError::InvalidInput { detail, .. } => (detail, None),
            CoreError::ConfirmationRequired { .. }
            | CoreError::Locked
            | CoreError::WrongPassword
            | CoreError::WrongPassphrase
            | CoreError::PassphraseRequired
            | CoreError::NotFound { .. }
            | CoreError::AlreadyExists { .. }
            | CoreError::Unsupported(_)
            | CoreError::UnknownPendingTx(_)
            | CoreError::RebuildRequired
            | CoreError::AddressMismatch { .. }
            | CoreError::Storage { .. }
            | CoreError::Keychain { .. }
            | CoreError::Internal { .. } => (copy.to_owned(), None),
        };
        Self { code: IpcErrorCode::from(domain), message, detail }
    }
}

impl From<ChainError> for IpcError {
    fn from(error: ChainError) -> Self {
        Self::from(CoreError::Chain(error))
    }
}

impl From<StartupError> for IpcError {
    fn from(error: StartupError) -> Self {
        let message = error.to_string();
        let (code, detail) = match error {
            StartupError::SchemaTooNew { found, supported } => {
                (ErrorCode::Storage.into(), Some(IpcErrorDetail::SchemaTooNew { found, supported }))
            }
            StartupError::KeychainUnavailable => (ErrorCode::Keychain.into(), None),
            StartupError::SettingsInvalid { .. } | StartupError::NetworkSettings { .. } => {
                (ErrorCode::InvalidInput.into(), None)
            }
            StartupError::DatabaseLocked
            | StartupError::DataDirUnwritable { .. }
            | StartupError::Corrupt
            | StartupError::Failed { .. } => (ErrorCode::Storage.into(), None),
        };
        Self { code, message, detail }
    }
}

impl From<JoinError> for IpcError {
    fn from(error: JoinError) -> Self {
        if error.is_panic() {
            return Self::new(
                IpcErrorCode::Shell(ShellErrorCode::Panic),
                "Jelly encountered an internal error.",
            );
        }
        Self::internal("The operation was cancelled.")
    }
}

fn chain_detail(error: ChainError) -> Option<IpcErrorDetail> {
    match error {
        ChainError::EndpointsDown { chain, attempts, .. } => {
            Some(IpcErrorDetail::EndpointsDown { chain, attempts })
        }
        ChainError::Rejected(Rejection { kind, retry, .. }) => {
            Some(IpcErrorDetail::Rejected { rejection: kind, retry })
        }
        ChainError::InsufficientFunds { asset, needed, available } => {
            Some(IpcErrorDetail::InsufficientFunds { asset, needed, available })
        }
        ChainError::ProtocolTooNew { chain, version } => {
            Some(IpcErrorDetail::ProtocolTooNew { chain, version })
        }
        ChainError::Rpc(_)
        | ChainError::InvalidAddress(_)
        | ChainError::Signing(_)
        | ChainError::Unsupported(_)
        | ChainError::InvalidInput(..)
        | ChainError::Internal { .. } => None,
    }
}
