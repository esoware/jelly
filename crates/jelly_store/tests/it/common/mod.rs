use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use jelly_core::{
    Amount, AssetId, ChainId, CursorScope, DerivationPath, KeySource, PassphrasePolicy, ScanStream,
    TxDirection, TxId, TxRecord, WalletColor, test_util::FakeClock,
};
use jelly_store::{AddressRow, NewAddress, NewWallet, Recovered, Store, StoreError, WalletRow};
use tempfile::TempDir;

const DB_FILE: &str = "jelly.sqlite";

pub(crate) const SCHEMA_VERSION: i64 = 1;

pub(crate) type Fallible<T> = Result<T, Box<dyn Error + Send + Sync>>;

/// A real temp-dir file, so every test exercises the shipped pragmas.
#[derive(Debug)]
pub(crate) struct Harness {
    pub(crate) store: Store,
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) recovered: Recovered,
    pub(crate) directory: PathBuf,
    /// Declared last so the store's connections drop before the directory is removed.
    scratch: TempDir,
}

pub(crate) async fn harness() -> Fallible<Harness> {
    let scratch = tempfile::tempdir()?;
    let directory = scratch.path().to_path_buf();
    let clock = Arc::new(FakeClock::default());
    let (store, recovered) = open_in(&directory, &clock).await?;
    Ok(Harness { store, clock, recovered, directory, scratch })
}

/// The store is closed but the temp directory stays alive, so a test can inspect or
/// damage the file before reopening it.
#[derive(Debug)]
pub(crate) struct Closed {
    pub(crate) clock: Arc<FakeClock>,
    pub(crate) directory: PathBuf,
    scratch: TempDir,
}

impl Harness {
    pub(crate) async fn close(self) -> Fallible<Closed> {
        let Self { store, clock, directory, scratch, .. } = self;
        store.close().await?;
        Ok(Closed { clock, directory, scratch })
    }
}

impl Closed {
    pub(crate) async fn open(self) -> Fallible<Harness> {
        let Self { clock, directory, scratch } = self;
        let (store, recovered) = open_in(&directory, &clock).await?;
        Ok(Harness { store, clock, recovered, directory, scratch })
    }
}

pub(crate) async fn open_in(
    directory: &Path,
    clock: &Arc<FakeClock>,
) -> Result<(Store, Recovered), StoreError> {
    Store::open(directory, Arc::<FakeClock>::clone(clock)).await
}

pub(crate) fn db_path(directory: &Path) -> PathBuf {
    directory.join(DB_FILE)
}

pub(crate) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(suffix);
    PathBuf::from(raw)
}

pub(crate) fn new_wallet(name: &str) -> NewWallet {
    NewWallet {
        name: name.to_owned(),
        color: WalletColor::default(),
        passphrase_policy: PassphrasePolicy::None,
        account_index: 0,
    }
}

pub(crate) fn new_address(wallet_id: i64, chain: ChainId, address: &str) -> NewAddress {
    NewAddress {
        wallet_id,
        chain,
        address: address.to_owned(),
        source: KeySource::Derived { path: DerivationPath::new("m/84'/0'/0'/0/0") },
        script_kind: None,
        derivation_index: Some(0),
        label: None,
    }
}

pub(crate) async fn wallet(store: &Store, name: &str) -> Fallible<WalletRow> {
    Ok(store.create_wallet(new_wallet(name)).await?)
}

pub(crate) async fn address(
    store: &Store,
    wallet_id: i64,
    chain: ChainId,
    address: &str,
) -> Fallible<AddressRow> {
    Ok(store.insert_address(new_address(wallet_id, chain, address)).await?)
}

/// A timestamp makes the record mined. Without one it is pending, and the file orders it
/// by when it was first stored.
pub(crate) fn record(address_id: i64, txid: &str, timestamp: Option<i64>) -> TxRecord {
    let builder = TxRecord::builder(
        AssetId::Native(ChainId::Bitcoin),
        address_id,
        TxId::new(ChainId::Bitcoin, txid),
        TxDirection::Incoming,
        Amount::from_base_units(1_000),
    );
    match timestamp {
        Some(stamp) => builder.confirmed_at(800_000, Some(stamp), 800_000).build(),
        None => builder.build(),
    }
}

pub(crate) async fn scan(
    store: &Store,
    address_id: i64,
    stream: ScanStream,
) -> Fallible<Option<Vec<u8>>> {
    let rows = store.load_scans(vec![CursorScope::Address(address_id)], vec![stream]).await?;
    Ok(rows.first().map(|row| row.cursor.with_bytes(<[u8]>::to_vec)))
}
