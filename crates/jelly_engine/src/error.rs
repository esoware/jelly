use std::io;

use jelly_core::{BoxedSource, CoreError};
use jelly_store::StoreError;
use jelly_vault::VaultError;
use tokio::task::{JoinError, spawn_blocking};

/// Why the app couldn't start.
///
/// Startup runs on a background task and never panics, so every failure it can hit is a
/// variant here.
#[derive(Debug, thiserror::Error)]
pub enum StartupError {
    #[error("The wallet database is schema v{found}, but this build supports v{supported}.")]
    SchemaTooNew { found: u32, supported: u32 },
    #[error("Another instance already has the wallet database open.")]
    DatabaseLocked,
    #[error("The data directory cannot be written to.")]
    DataDirUnwritable {
        #[source]
        source: io::Error,
    },
    #[error("The platform credential store did not start.")]
    KeychainUnavailable,
    #[error("The wallet database is damaged beyond recovery.")]
    Corrupt,
    #[error("The settings file is not valid.")]
    SettingsInvalid {
        #[source]
        source: BoxedSource,
    },
    #[error("The saved network settings cannot be used.")]
    NetworkSettings {
        #[source]
        source: BoxedSource,
    },
    #[error("Startup failed while {context}.")]
    Failed {
        context: &'static str,
        #[source]
        source: BoxedSource,
    },
}

impl From<StoreError> for StartupError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::SchemaTooNew { found, supported } => {
                Self::SchemaTooNew { found, supported }
            }
            StoreError::Corrupt => Self::Corrupt,
            StoreError::Busy => Self::DatabaseLocked,
            StoreError::Io { source, .. } => Self::DataDirUnwritable { source },
            other @ (StoreError::NotFound { .. }
            | StoreError::AlreadyExists { .. }
            | StoreError::MissingReference { .. }
            | StoreError::Rejected { .. }
            | StoreError::Decode { .. }
            | StoreError::Overflow { .. }
            | StoreError::Clock(_)
            | StoreError::Sqlite { .. }
            | StoreError::Internal { .. }
            | StoreError::Task(_)) => {
                Self::Failed { context: "opening the wallet database", source: Box::new(other) }
            }
        }
    }
}

impl From<VaultError> for StartupError {
    fn from(error: VaultError) -> Self {
        match error {
            VaultError::KeychainUnavailable | VaultError::Keychain(_) => Self::KeychainUnavailable,
            other @ (VaultError::WrongPassword
            | VaultError::FileNotFound(_)
            | VaultError::AlreadyExists(_)
            | VaultError::KeychainMissing
            | VaultError::KeychainOccupied
            | VaultError::KeychainMalformed
            | VaultError::Malformed(_)
            | VaultError::Corrupt { .. }
            | VaultError::UnsupportedVersion { .. }
            | VaultError::UnsupportedPayloadVersion { .. }
            | VaultError::WeakKdfParams { .. }
            | VaultError::KdfParams(_)
            | VaultError::InvalidEntryId
            | VaultError::EntryNotFound(_)
            | VaultError::NoEntryOfKind(_)
            | VaultError::NotKeyMaterial(_)
            | VaultError::Io { .. }
            | VaultError::Entropy(_)
            | VaultError::Internal { .. }) => {
                Self::Failed { context: "checking the credential store", source: Box::new(other) }
            }
        }
    }
}

pub(crate) fn task_error(error: JoinError) -> CoreError {
    CoreError::internal("an engine background task did not finish", error)
}

/// Runs `work` on the blocking pool, for key derivation, vault crypto, and file writes
/// that don't belong on a runtime worker.
pub(crate) async fn blocking<T, E>(
    work: impl FnOnce() -> Result<T, E> + Send + 'static,
) -> Result<T, CoreError>
where
    T: Send + 'static,
    E: Send + 'static,
    CoreError: From<E>,
{
    Ok(spawn_blocking(work).await.map_err(task_error)??)
}
