use std::path::Path;
use std::sync::Arc;

use jelly_core::{
    Amount, AssetId, ChainId, PhraseLength, TxPreview, WalletColor, test_util::FakeClock,
};
use jelly_vault::test_util::MemoryKeyStore;
use secrecy::SecretString;
use tempfile::TempDir;

use crate::test_util::{NullAdapterBuilder, config};
use crate::{EngineDeps, PassphraseInput, WalletEngine, WalletRecord};

pub(crate) fn password() -> SecretString {
    SecretString::from("test wallet password")
}

/// The preview the frozen digest in `digest.rs` was taken from.
pub(crate) fn preview() -> TxPreview {
    TxPreview {
        chain: ChainId::Bitcoin,
        asset: AssetId::Native(ChainId::Bitcoin),
        from: vec![String::from("bc1qfrom")],
        to: String::from("bc1qto"),
        destination_tag: None,
        amount: Amount::from_base_units(10_000),
        fee: Amount::from_base_units(250),
        rent: None,
        details: Vec::new(),
        warnings: Vec::new(),
    }
}

/// An engine's ports on `directory`: a fake clock, an in-memory credential store, and a
/// builder that builds nothing.
pub(crate) fn deps(directory: &Path) -> EngineDeps {
    EngineDeps {
        data_dir: directory.to_path_buf(),
        clock: Arc::new(FakeClock::default()),
        keystore: Arc::new(MemoryKeyStore::new()),
        builder: Arc::new(NullAdapterBuilder),
        table: jelly_chains::chains(),
    }
}

/// A new seed wallet, left open.
pub(crate) async fn seed_wallet(engine: &WalletEngine, name: &str) -> WalletRecord {
    engine
        .create_seed_wallet(
            name,
            WalletColor::default(),
            password(),
            PhraseLength::Twelve,
            PassphraseInput::None,
        )
        .await
        .unwrap()
        .wallet
}

#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) engine: WalletEngine,
    /// Declared last so the engine drops before the directory is removed.
    _directory: TempDir,
}

pub(crate) async fn harness() -> Harness {
    let directory = tempfile::tempdir().unwrap();
    let started = WalletEngine::start(config(), deps(directory.path())).await.unwrap();
    Harness { engine: started.engine, _directory: directory }
}
