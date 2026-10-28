use std::path::Path;
use std::sync::Arc;

use jelly_core::{
    ChainId, Clock, CursorBytes, CursorScope, EndpointHealthRecord, EndpointOverride,
    HeldReservation, ReservationSlot, ScanAdvance, ScanStream, WalletColor,
};
use rusqlite::Connection;
use tokio::sync::Mutex;
use tokio::task::spawn_blocking;

use crate::address::{AddressRow, NewAddress};
use crate::balance::{AssetTotal, BalanceRow};
use crate::batch::SyncBatch;
use crate::contact::{ContactEdit, ContactRow, NewContact};
use crate::db::{self, ReadPool, Recovered};
use crate::error::StoreError;
use crate::pin::PinRow;
use crate::tx::{TxFilter, TxGroupFilter, TxGroupPage, TxPage, TxWrite};
use crate::wallet::{ChainStatePatch, ChainStateRow, NewWallet, WalletRow, WalletSettingsRow};

/// The wallet database. Every query runs on the blocking pool, off the runtime's workers.
#[derive(Debug)]
pub struct Store {
    /// Taken before a write goes to the blocking pool and released there after the commit,
    /// so the next holder sees every write already dispatched.
    writer: Arc<Mutex<Connection>>,
    readers: Arc<ReadPool>,
    clock: Arc<dyn Clock>,
}

/// One connection inside one snapshot.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    connection: &'a Connection,
}

impl Reader<'_> {
    pub(crate) const fn connection(&self) -> &Connection {
        self.connection
    }
}

/// One transaction, with one timestamp for every row it writes.
#[derive(Debug)]
pub struct Writer<'a> {
    connection: &'a Connection,
    now_unix_secs: i64,
}

impl Writer<'_> {
    pub(crate) const fn connection(&self) -> &Connection {
        self.connection
    }

    pub(crate) const fn now_unix_secs(&self) -> i64 {
        self.now_unix_secs
    }
}

/// Declares async `Store` methods that run a [`Reader`] or [`Writer`] method through
/// [`Store::read`] or [`Store::transact`]. Each body calls the method it wraps, so the two
/// can't drift apart.
macro_rules! forward {
    ($via:ident => $(
        $(#[$meta:meta])*
        fn $name:ident ( $($arg:ident : $ty:ty),* $(,)? ) -> $out:ty = |$scope:ident| $call:expr;
    )*) => {
        impl Store {
            $(
                $(#[$meta])*
                pub async fn $name(&self, $($arg: $ty),*) -> Result<$out, StoreError> {
                    self.$via(move |$scope| $call).await
                }
            )*
        }
    };
}

forward! { read =>
    fn get_wallet(wallet_id: i64) -> WalletRow = |reader| reader.get_wallet(wallet_id);
    fn list_wallets() -> Vec<WalletRow> = |reader| reader.list_wallets();
    fn get_wallet_settings(wallet_id: i64) -> WalletSettingsRow =
        |reader| reader.get_wallet_settings(wallet_id);
    fn list_chain_states(wallet_id: i64) -> Vec<ChainStateRow> = |reader| reader.list_chain_states(wallet_id);

    fn get_address(address_id: i64) -> AddressRow = |reader| reader.get_address(address_id);
    fn list_addresses(wallet_id: i64, chain: Option<ChainId>) -> Vec<AddressRow> =
        |reader| reader.list_addresses(wallet_id, chain);

    fn get_contact(wallet_id: i64, contact_id: i64) -> ContactRow =
        |reader| reader.get_contact(wallet_id, contact_id);
    fn list_contacts(wallet_id: i64) -> Vec<ContactRow> = |reader| reader.list_contacts(wallet_id);

    fn list_balances(wallet_id: i64) -> Vec<BalanceRow> = |reader| reader.list_balances(wallet_id);
    fn balance_totals(wallet_id: i64) -> Vec<AssetTotal> = |reader| reader.balance_totals(wallet_id);

    /// One page of history, newest first.
    fn list_txs(filter: TxFilter) -> TxPage = |reader| reader.list_txs(filter);
    /// One page of history with one entry per transaction and asset, newest first.
    fn list_tx_groups(filter: TxGroupFilter) -> TxGroupPage = |reader| reader.list_tx_groups(filter);

    fn load_scans(scopes: Vec<CursorScope>, streams: Vec<ScanStream>) -> Vec<ScanAdvance> =
        |reader| reader.load_scans(&scopes, &streams);

    fn list_endpoints() -> Vec<(ChainId, EndpointOverride)> = |reader| reader.list_endpoints();
    fn list_endpoint_health() -> Vec<EndpointHealthRecord> = |reader| reader.list_endpoint_health();

    fn list_pins() -> Vec<PinRow> = |reader| reader.list_pins();
}

forward! { transact =>
    fn create_wallet(new: NewWallet) -> WalletRow = |writer| writer.create_wallet(&new);
    fn update_wallet(wallet_id: i64, name: String, color: WalletColor) -> WalletRow =
        |writer| writer.update_wallet(wallet_id, &name, &color);
    fn delete_wallet(wallet_id: i64) -> () = |writer| writer.delete_wallet(wallet_id);
    fn set_chain_state(wallet_id: i64, chain: ChainId, patch: ChainStatePatch) -> () =
        |writer| writer.set_chain_state(wallet_id, chain, patch);

    fn insert_address(new: NewAddress) -> AddressRow = |writer| writer.insert_address(&new);
    fn set_address_label(address_id: i64, label: Option<String>) -> AddressRow =
        |writer| writer.set_address_label(address_id, label.as_deref());
    fn delete_address(address_id: i64) -> () = |writer| writer.delete_address(address_id);

    fn insert_contact(new: NewContact) -> ContactRow = |writer| writer.insert_contact(&new);
    fn update_contact(wallet_id: i64, contact_id: i64, edit: ContactEdit) -> ContactRow =
        |writer| writer.update_contact(wallet_id, contact_id, &edit);
    fn delete_contact(wallet_id: i64, contact_id: i64) -> () =
        |writer| writer.delete_contact(wallet_id, contact_id);

    fn reserve(scope: CursorScope, slot: ReservationSlot, payload: CursorBytes) -> () =
        |writer| writer.reserve(scope, slot, &payload);
    fn forget_reconstructible_scans(address_ids: Vec<i64>) -> () = |writer| writer.forget_reconstructible_scans(&address_ids);

    fn add_endpoint(chain: ChainId, url: String, display: String) -> () =
        |writer| writer.add_endpoint(chain, &url, &display);
    fn set_endpoint_enabled(chain: ChainId, url: String, is_enabled: bool) -> () =
        |writer| writer.set_endpoint_enabled(chain, &url, is_enabled);
    fn set_endpoint_pinned(chain: ChainId, url: String, is_pinned: bool) -> () =
        |writer| writer.set_endpoint_pinned(chain, &url, is_pinned);
    fn delete_endpoint(chain: ChainId, url: String) -> () = |writer| writer.delete_endpoint(chain, &url);

    fn put_pin(row: PinRow) -> () = |writer| writer.put_pin(&row);

    fn apply_sync_batch(batch: SyncBatch) -> Vec<TxWrite> =
        |writer| writer.apply_sync_batch(&batch);
}

impl Store {
    /// Opens `jelly.sqlite` in `directory`, or creates it with the current schema.
    ///
    /// A damaged file is moved aside for a fresh one, and [`Recovered`] reports it. A file
    /// from a newer build is refused.
    pub async fn open(
        directory: &Path,
        clock: Arc<dyn Clock>,
    ) -> Result<(Self, Recovered), StoreError> {
        let directory = directory.to_path_buf();
        blocking(move || {
            let (writer, readers, recovered) = db::open(&directory, &*clock)?;
            let store =
                Self { writer: Arc::new(Mutex::new(writer)), readers: Arc::new(readers), clock };
            Ok((store, recovered))
        })
        .await
    }

    /// Checkpoints the write-ahead log, so the next open has nothing to replay.
    pub async fn close(self) -> Result<(), StoreError> {
        let Self { writer, readers, .. } = self;
        let writer = writer.lock_owned().await;
        blocking(move || db::close(&writer, &readers)).await
    }

    /// Runs `work` in one transaction. Every row it writes carries the same timestamp, read
    /// before the call waits for the writer.
    pub async fn transact<T, F>(&self, work: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Writer<'_>) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let now_unix_secs = self.clock.now_secs()?;
        let mut writer = Arc::clone(&self.writer).lock_owned().await;
        // The guard moves into the blocking task, so canceling the caller can't release
        // the writer before the commit.
        blocking(move || {
            db::write(&mut writer, |connection| work(&Writer { connection, now_unix_secs }))
        })
        .await
    }

    /// Waits for the writer first, so a replacement adapter sees reservations whose callers
    /// were canceled after dispatching them.
    pub async fn load_reservations(
        &self,
        scopes: Vec<CursorScope>,
        slots: Vec<ReservationSlot>,
    ) -> Result<Vec<HeldReservation>, StoreError> {
        let _writer = self.writer.lock().await;
        self.read(move |reader| reader.load_reservations(&scopes, &slots)).await
    }

    async fn read<T, F>(&self, work: F) -> Result<T, StoreError>
    where
        F: FnOnce(&Reader<'_>) -> Result<T, StoreError> + Send + 'static,
        T: Send + 'static,
    {
        let readers = Arc::clone(&self.readers);
        blocking(move || readers.read(|connection| work(&Reader { connection }))).await
    }
}

async fn blocking<T, F>(work: F) -> Result<T, StoreError>
where
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
    T: Send + 'static,
{
    spawn_blocking(work).await?
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use jelly_core::{
        ChainId, Clock as _, DerivationPath, KeySource, PassphrasePolicy, WalletColor,
        test_util::FakeClock,
    };
    use tempfile::TempDir;

    use super::Store;
    use crate::address::NewAddress;
    use crate::error::StoreError;
    use crate::wallet::{ChainStatePatch, NewWallet};

    const fn assert_send_sync<T: Send + Sync>() {}

    // Reader and Writer are deliberately neither, because each borrows a connection
    // that never leaves its blocking task.
    const _: () = assert_send_sync::<Store>();

    /// A store on a fresh file. The directory is returned so it outlives the store.
    async fn opened(clock: &Arc<FakeClock>) -> (TempDir, Store) {
        let scratch = tempfile::tempdir().unwrap();
        let (store, _) = Store::open(scratch.path(), Arc::<FakeClock>::clone(clock)).await.unwrap();
        (scratch, store)
    }

    fn new_wallet(name: &str) -> NewWallet {
        NewWallet {
            name: name.to_owned(),
            color: WalletColor::default(),
            passphrase_policy: PassphrasePolicy::None,
            account_index: 0,
        }
    }

    #[tokio::test]
    async fn one_unit_of_work_can_create_a_wallet_with_its_first_address() {
        let clock = Arc::new(FakeClock::default());
        let (_scratch, store) = opened(&clock).await;
        let now = clock.now_secs().unwrap();

        let (wallet, address) = store
            .transact(|writer| {
                let wallet = writer.create_wallet(&new_wallet("first run"))?;
                let address = writer.insert_address(&NewAddress {
                    wallet_id: wallet.id,
                    chain: ChainId::Bitcoin,
                    address: String::from("bc1qunit"),
                    source: KeySource::Derived { path: DerivationPath::new("m/84'/0'/0'/0/0") },
                    script_kind: None,
                    derivation_index: Some(0),
                    label: None,
                })?;
                Ok((wallet, address))
            })
            .await
            .unwrap();
        assert_eq!(wallet.created_at_unix_secs, now);
        assert_eq!(address.created_at_unix_secs, now);
        assert_eq!(address.derivation_index, Some(0));
        assert_eq!(store.get_wallet_settings(wallet.id).await.unwrap().account_index, 0);

        let doomed = wallet.id;
        store.transact(move |writer| writer.delete_wallet(doomed)).await.unwrap();
        assert!(store.list_wallets().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_unit_of_work_that_fails_leaves_nothing_behind() {
        let (_scratch, store) = opened(&Arc::new(FakeClock::default())).await;

        let outcome: Result<(), StoreError> = store
            .transact(|writer| {
                let wallet = writer.create_wallet(&new_wallet("half")).unwrap();
                writer
                    .set_chain_state(wallet.id, ChainId::Bsc, ChainStatePatch::default())
                    .unwrap();
                Err(StoreError::Internal { context: "the unit of work failed" })
            })
            .await;
        assert!(matches!(outcome, Err(StoreError::Internal { .. })), "{outcome:?}");
        assert!(store.list_wallets().await.unwrap().is_empty());
    }
}
