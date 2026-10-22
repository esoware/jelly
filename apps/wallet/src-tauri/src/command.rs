//! IPC handlers wait for startup, delegate to the engine or services, and map errors for
//! the frontend.
//!
//! A startup timeout returns `Starting` without attempting the command. Work runs in a
//! joined task so a panic returns `Panic` rather than leaving the frontend promise
//! unresolved.
//!
//! IPC deserializes secret arguments into plain strings. Handlers move them into `secrecy`
//! or `zeroize` types before use. Serialization can leave unwipeable copies, so secret
//! exports require separate authorization.

#![expect(
    clippy::unreachable,
    reason = "tauri::command expands to a dispatch arm whose span is the wrapped function"
)]

pub(crate) mod address;
pub(crate) mod app;
pub(crate) mod chain;
pub(crate) mod contact;
pub(crate) mod endpoint;
pub(crate) mod export;
pub(crate) mod money;
pub(crate) mod setting;
pub(crate) mod tx;
pub(crate) mod wallet;

use std::fmt;
use std::sync::Arc;

use jelly_core::CoreError;
use jelly_engine::WalletEngine;
use serde::{Serialize, Serializer};
use specta::Type;
use tokio::task::spawn_blocking;
use zeroize::Zeroizing;

use crate::error::IpcError;
use crate::services::Services;
use crate::state::AppState;

const TARGET: &str = "jelly::app::command";

/// A secret a command hands the interface, kept in its zeroizing buffer until serde writes
/// it out.
#[derive(Type)]
#[specta(transparent, inline)]
pub(crate) struct RevealedSecret(#[specta(type = String)] Zeroizing<String>);

impl Serialize for RevealedSecret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_str())
    }
}

impl fmt::Debug for RevealedSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RevealedSecret(<redacted>)")
    }
}

impl From<Zeroizing<String>> for RevealedSecret {
    fn from(secret: Zeroizing<String>) -> Self {
        Self(secret)
    }
}

/// Waits for the engine and joins command work so panics become IPC errors.
async fn with_engine<T, Fut>(
    state: &AppState,
    work: impl FnOnce(WalletEngine) -> Fut,
) -> Result<T, IpcError>
where
    Fut: Future<Output = Result<T, CoreError>> + Send + 'static,
    T: Send + 'static,
{
    let engine = state.engine().await?;
    tokio::spawn(work(engine)).await?.map_err(IpcError::from)
}

/// Waits for services and joins command work so panics become IPC errors.
async fn with_services<T, Fut>(
    state: &AppState,
    work: impl FnOnce(Arc<Services>) -> Fut,
) -> Result<T, IpcError>
where
    Fut: Future<Output = Result<T, IpcError>> + Send + 'static,
    T: Send + 'static,
{
    let services = state.services().await?;
    tokio::spawn(work(services)).await?
}

/// Opens a built-in explorer page in the system's default browser, which can block while
/// it starts.
async fn open_explorer(url: String) -> Result<(), IpcError> {
    spawn_blocking(move || {
        tauri_plugin_opener::open_url(url, None::<&str>)
            .map_err(|_| IpcError::internal("Could not open the browser."))
    })
    .await?
}

#[cfg(test)]
mod tests {
    use zeroize::Zeroizing;

    use super::RevealedSecret;

    const PHRASE: &str =
        "legal winner thank year wave sausage worth useful legal winner thank yellow";

    #[test]
    fn debug_of_a_revealed_secret_leaves_out_the_secret() {
        let revealed = RevealedSecret::from(Zeroizing::new(String::from(PHRASE)));
        let rendered = format!("{revealed:?}");
        assert!(!rendered.contains("sausage"), "the revealed secret reached a debug rendering");
    }

    #[test]
    fn a_revealed_secret_serializes_as_its_bare_text() {
        let revealed = RevealedSecret::from(Zeroizing::new(String::from(PHRASE)));
        assert_eq!(serde_json::to_value(&revealed).unwrap(), PHRASE);
    }
}
