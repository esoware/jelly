use std::{fmt, io};

use jelly_core::{
    Amount, AssetId, ChainError, ChainId, CoreError, EndpointAttempt, ErrorCode, Rejection,
    RejectionKind, RetryPolicy, SkipReason, wire_enum,
};
use jelly_engine::StartupError;
use serde::Serialize;
use specta::Type;
use tokio::task::JoinError;

/// Failures that stop the app from running. They never cross IPC.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("the async runtime couldn't be built")]
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
        /// The price provider didn't answer, or prices are off.
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

/// Amounts stay in base units so the frontend can apply the asset's decimals.
///
/// `NoAdapter` has no reason when the chain is switched off or the wallet's adapters aren't
/// built yet.
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

/// `message` is copy. It never holds a secret, a raw URL, a file path, or a library's error
/// text. The frontend branches on `code` and `detail`, never on the wording.
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

    /// Takes fixed copy. Log the details separately.
    pub(crate) fn internal(message: &'static str) -> Self {
        Self::new(ErrorCode::Internal.into(), message)
    }
}

/// Only invalid input carries its own text, which is copy by contract. Every other error
/// gets a fixed fallback, so node text, contexts, and base units stay in Rust.
impl From<CoreError> for IpcError {
    fn from(error: CoreError) -> Self {
        let code = IpcErrorCode::from(error.code());
        let detail = match error {
            CoreError::Chain(ChainError::InvalidInput(message, _))
            | CoreError::InvalidInput { detail: message, .. } => {
                return Self { code, message, detail: None };
            }
            CoreError::Chain(ChainError::EndpointsDown { chain, attempts, .. }) => {
                Some(IpcErrorDetail::EndpointsDown { chain, attempts })
            }
            CoreError::Chain(ChainError::Rejected(Rejection { kind, retry, .. })) => {
                Some(IpcErrorDetail::Rejected { rejection: kind, retry })
            }
            CoreError::Chain(ChainError::InsufficientFunds { asset, needed, available }) => {
                Some(IpcErrorDetail::InsufficientFunds { asset, needed, available })
            }
            CoreError::Chain(ChainError::ProtocolTooNew { chain, version }) => {
                Some(IpcErrorDetail::ProtocolTooNew { chain, version })
            }
            CoreError::NoAdapter { chain, reason } => {
                Some(IpcErrorDetail::NoAdapter { chain, reason })
            }
            CoreError::Chain(
                ChainError::Rpc(_)
                | ChainError::InvalidAddress(_)
                | ChainError::Signing(_)
                | ChainError::Unsupported(_)
                | ChainError::Internal { .. },
            )
            | CoreError::ConfirmationRequired { .. }
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
            | CoreError::Internal { .. } => None,
        };
        Self { code, message: String::from("Jelly could not complete the request."), detail }
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
        Self::internal("The operation was canceled.")
    }
}
