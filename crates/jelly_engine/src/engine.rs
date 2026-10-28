use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use jelly_core::{AdapterBuilder, ChainTable, Clock, CoreError, PinStore, WalletEvent};
use jelly_store::{Recovered, Store};
use jelly_vault::MasterKeyStore;
use serde::Serialize;
use tokio::sync::{Mutex, broadcast};
use tokio::task::spawn_blocking;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::chains::Chains;
use crate::config::EngineConfig;
use crate::error::StartupError;
use crate::events::Events;
use crate::pending::PendingTxs;
use crate::pin::{self, PinCache};
use crate::pump;
use crate::session::{LockReason, SessionSlot};
use crate::settings::{Settings, auto_lock_duration};

/// The data directory, and the ports the app shell provides because the engine can't
/// build them itself.
#[derive(Debug, Clone)]
pub struct EngineDeps {
    pub data_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub keystore: Arc<dyn MasterKeyStore>,
    pub builder: Arc<dyn AdapterBuilder>,
    /// Every chain's entry, which the engine derives keys and addresses through.
    pub table: &'static ChainTable,
}

/// The wallet database couldn't be used, so it was moved aside and this run started with
/// an empty one.
///
/// Balances and history re-sync, but wallets and their settings, chain state, addresses,
/// contacts, endpoints, and pins exist only in the file at `moved_to`. The vault files are
/// still there but their wallets are no longer listed, so the user has to be told.
#[cfg_attr(feature = "ipc", derive(specta::Type))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseRebuilt {
    /// The old file's path for display. It's inside the data directory, which the app
    /// already shows.
    pub moved_to: String,
}

/// A started engine, and where the old database went if it was rebuilt.
#[derive(Debug)]
pub struct Started {
    pub engine: WalletEngine,
    pub rebuilt: Option<DatabaseRebuilt>,
}

/// A failed start, and the database it replaced before failing.
///
/// The next start opens the replacement and has nothing to report, so this is the only
/// chance to tell the user their wallets are no longer listed.
#[derive(Debug)]
pub struct StartFailure {
    pub error: StartupError,
    pub rebuilt: Option<DatabaseRebuilt>,
}

/// Cheap to clone. Every operation borrows it except `shutdown`, which consumes it.
#[derive(Clone)]
pub struct WalletEngine {
    pub(crate) inner: Arc<Inner>,
}

impl fmt::Debug for WalletEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WalletEngine")
            .field("is_unlocked", &self.inner.session.state().is_unlocked())
            .finish_non_exhaustive()
    }
}

/// There's no engine-wide lock. Each subsystem guards its own state.
pub(crate) struct Inner {
    pub(crate) store: Store,
    pub(crate) settings: Settings,
    /// The TLS pins the network layer verifies against. Its writer runs in `tasks`.
    pub(crate) pins: Arc<PinCache>,
    pub(crate) keystore: Arc<dyn MasterKeyStore>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) builder: Arc<dyn AdapterBuilder>,
    pub(crate) table: &'static ChainTable,
    pub(crate) config: EngineConfig,
    pub(crate) data_dir: PathBuf,
    pub(crate) session: SessionSlot,
    pub(crate) chains: Chains,
    pub(crate) pending: PendingTxs,
    /// Serializes a send's reservations, signing, and broadcast against adapter rebuilds
    /// and locks.
    pub(crate) sends: Arc<Mutex<()>>,
    pub(crate) events: Events,
    pub(crate) tasks: TaskTracker,
    pub(crate) cancel: CancellationToken,
}

impl fmt::Debug for Inner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Inner").field("data_dir", &self.data_dir).finish_non_exhaustive()
    }
}

impl WalletEngine {
    /// Opens or creates the database and the settings file, loads the TLS pins, and starts
    /// the event pump and the pin writer. The engine starts locked, with no key material.
    pub async fn start(config: EngineConfig, deps: EngineDeps) -> Result<Started, StartFailure> {
        let EngineDeps { data_dir, clock, keystore, builder, table } = deps;
        let (store, recovered) = Store::open(&data_dir, Arc::clone(&clock))
            .await
            .map_err(|error| StartFailure { error: error.into(), rebuilt: None })?;
        let rebuilt = match recovered {
            Recovered::No => None,
            Recovered::Rebuilt { moved_to } => {
                Some(DatabaseRebuilt { moved_to: moved_to.display().to_string() })
            }
        };
        let loaded = async {
            let settings_dir = data_dir.clone();
            let settings = spawn_blocking(move || Settings::load(&settings_dir)).await.map_err(
                |source| StartupError::Failed {
                    context: "loading the settings",
                    source: Box::new(source),
                },
            )??;
            let (pins, pin_writes) = pin::load(&store).await?;
            Ok::<_, StartupError>((settings, pins, pin_writes))
        }
        .await;
        let (settings, pins, pin_writes) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => return Err(StartFailure { error, rebuilt }),
        };
        let auto_lock = auto_lock_duration(settings.get().await.auto_lock_minutes);
        let (events, inbound) = Events::new(config.channel_capacity());

        let inner = Arc::new(Inner {
            store,
            settings,
            pins,
            keystore,
            clock,
            builder,
            table,
            config,
            data_dir,
            session: SessionSlot::new(auto_lock),
            chains: Chains::default(),
            pending: PendingTxs::default(),
            sends: Arc::new(Mutex::new(())),
            events,
            tasks: TaskTracker::new(),
            cancel: CancellationToken::new(),
        });

        let engine = Self { inner };
        engine.inner.tasks.spawn(pump::run(
            Arc::downgrade(&engine.inner),
            inbound,
            engine.inner.config.clone(),
            engine.inner.cancel.clone(),
        ));
        engine.inner.tasks.spawn(pin::persist(
            Arc::downgrade(&engine.inner),
            pin_writes,
            engine.inner.cancel.clone(),
        ));
        Ok(Started { engine, rebuilt })
    }

    /// The TLS pins the network layer verifies against. It holds no engine handle, so it
    /// can outlive the engine.
    pub fn pin_store(&self) -> Arc<dyn PinStore> {
        Arc::<PinCache>::clone(&self.inner.pins)
    }

    /// Every event the engine publishes, from the pump or sent directly. A slow
    /// subscriber lags instead of stalling the sync pass.
    pub fn subscribe(&self) -> broadcast::Receiver<WalletEvent> {
        self.inner.events.subscribe()
    }

    /// Locks the session, stops every task, and checkpoints the database.
    ///
    /// Locking comes first, so the keys are wiped even if a task never finishes. If
    /// another engine handle survives, the database stays open and we log it instead of
    /// waiting.
    pub async fn shutdown(self) -> Result<(), CoreError> {
        {
            let _transition = self.inner.session.transition.lock().await;
            self.lock_inner(LockReason::Requested).await;
            self.inner.cancel.cancel();
        }
        self.inner.tasks.close();
        self.inner.tasks.wait().await;

        let Self { inner } = self;
        let Ok(inner) = Arc::try_unwrap(inner) else {
            tracing::warn!(
                target: "jelly::engine",
                "the database was left open because another engine handle survives"
            );
            return Ok(());
        };
        Ok(inner.store.close().await?)
    }
}

const fn assert_send_sync<T: Send + Sync>() {}
const _: () = assert_send_sync::<WalletEngine>();
const _: () = assert_send_sync::<Started>();
