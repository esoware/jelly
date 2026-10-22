use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jelly_core::SystemClock;
use jelly_engine::test_util;
use jelly_lib::{ShellConfig, ShellDeps};
use jelly_prices::PriceFeedConfig;
use jelly_vault::test_util::{FailingKeyStore, MemoryKeyStore};
use secrecy::SecretString;
use serde_json::Value;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error>>;

pub(crate) const PASSWORD: &str = "correct horse battery staple";

pub(crate) fn secret(value: &str) -> SecretString {
    SecretString::from(value.to_owned())
}

pub(crate) fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(crate) fn tauri_conf() -> Fallible<Value> {
    let text = fs::read_to_string(manifest_dir().join("tauri.conf.json"))?;
    Ok(serde_json::from_str(&text)?)
}

/// Asks the price feed for nothing and uses the Argon2id test floor. Startup still uses the
/// real database and network configuration but opens no sockets.
pub(crate) fn offline_config() -> ShellConfig {
    ShellConfig {
        engine: test_util::config(),
        prices: PriceFeedConfig { tickers: BTreeSet::new() },
    }
}

/// Uses an in-memory credential store that never touches the platform keychain.
pub(crate) fn deps(data_dir: &Path) -> ShellDeps {
    ShellDeps {
        data_dir: data_dir.to_path_buf(),
        clock: Arc::new(SystemClock),
        keystore: Arc::new(MemoryKeyStore::new()),
    }
}

pub(crate) fn deps_without_keychain(data_dir: &Path) -> ShellDeps {
    ShellDeps {
        data_dir: data_dir.to_path_buf(),
        clock: Arc::new(SystemClock),
        keystore: Arc::new(FailingKeyStore),
    }
}
