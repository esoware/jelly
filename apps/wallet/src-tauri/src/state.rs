use std::fmt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use jelly_chains::AdapterFactory;
use jelly_core::ProxySetting;
use jelly_engine::{DatabaseRebuilt, EngineConfig, StartFailure, StartupError, WalletEngine};
use jelly_net::HttpClient;
use serde::Serialize;
use specta::Type;
use tauri::async_runtime;
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::error::IpcError;
use crate::services::{Services, ShellDeps, proxy_refused};

const TARGET: &str = "jelly::app::state";

/// How long a command waits for startup, so a stalled startup can't leave IPC promises
/// unsettled.
const READY_WAIT: Duration = Duration::from_secs(20);

/// The ready state owns services, so readiness can't disagree with their availability.
#[derive(Debug, Clone)]
pub(crate) enum Phase {
    Starting,
    Ready(Arc<Services>),
    /// Commands waiting for services return this startup error.
    Failed {
        error: IpcError,
        can_repair_proxy: bool,
        rebuilt: Option<DatabaseRebuilt>,
    },
}

impl Phase {
    pub(crate) const fn is_terminal(&self) -> bool {
        !matches!(*self, Self::Starting)
    }

    pub(crate) fn status(&self) -> StartupStatus {
        match *self {
            Self::Starting => StartupStatus::Starting,
            Self::Ready(ref services) => {
                StartupStatus::Ready { rebuilt: services.rebuilt().cloned() }
            }
            Self::Failed { ref error, can_repair_proxy, ref rebuilt } => StartupStatus::Failed {
                error: error.clone(),
                can_repair_proxy,
                rebuilt: rebuilt.clone(),
            },
        }
    }
}

/// Engine commands wait a bounded time for `Ready` or `Failed`. Both also report whether
/// this start replaced the database, since a start can replace it and then fail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartupStatus {
    Starting,
    #[serde(rename_all = "camelCase")]
    Ready {
        rebuilt: Option<DatabaseRebuilt>,
    },
    #[serde(rename_all = "camelCase")]
    Failed {
        error: IpcError,
        can_repair_proxy: bool,
        rebuilt: Option<DatabaseRebuilt>,
    },
}

/// One lifecycle channel shared by command waits, status queries, and startup events.
pub struct AppState {
    /// What startup hands to [`Services::start`], kept so a proxy repair opens the same
    /// data directory through the same credential store.
    deps: ShellDeps,
    phase: watch::Sender<Phase>,
    /// Owns startup and proxy repair, including the repair engine's shutdown.
    startup: TaskTracker,
    cancel_startup: CancellationToken,
    proxy_repair: Arc<AsyncMutex<()>>,
}

impl fmt::Debug for AppState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppState")
            .field("data_dir", &self.deps.data_dir)
            .field("phase", &self.phase.borrow().status())
            .finish_non_exhaustive()
    }
}

impl AppState {
    pub fn new(deps: ShellDeps) -> Self {
        Self {
            deps,
            phase: watch::Sender::new(Phase::Starting),
            startup: TaskTracker::new(),
            cancel_startup: CancellationToken::new(),
            proxy_repair: Arc::new(AsyncMutex::new(())),
        }
    }

    pub(crate) fn data_dir(&self) -> &Path {
        &self.deps.data_dir
    }

    pub fn status(&self) -> StartupStatus {
        self.phase.borrow().status()
    }

    pub(crate) fn phases(&self) -> watch::Receiver<Phase> {
        self.phase.subscribe()
    }

    /// Doesn't wait, for callers such as exit that can't block on startup.
    pub(crate) fn services_now(&self) -> Option<Arc<Services>> {
        match *self.phase.borrow() {
            Phase::Ready(ref services) => Some(Arc::clone(services)),
            Phase::Starting | Phase::Failed { .. } => None,
        }
    }

    /// Waits up to `READY_WAIT` and returns the startup error on failure.
    pub(crate) async fn services(&self) -> Result<Arc<Services>, IpcError> {
        let mut phases = self.phases();
        let settle = async {
            let settled = phases.wait_for(Phase::is_terminal).await.map(|phase| phase.clone());
            match settled {
                Ok(Phase::Ready(services)) => Ok(services),
                Ok(Phase::Failed { error, .. }) => Err(error),
                // Unreachable: `self` holds the sender, and only a terminal phase ends the wait.
                Ok(Phase::Starting) | Err(_) => {
                    Err(IpcError::internal("Jelly did not finish starting."))
                }
            }
        };
        timeout(READY_WAIT, settle).await.unwrap_or_else(|_| Err(IpcError::starting()))
    }

    pub async fn engine(&self) -> Result<WalletEngine, IpcError> {
        self.services().await?.engine()
    }

    /// Startup remains failed until the app is reopened. No network or price
    /// service exists during this repair, and the wallet stays locked.
    pub async fn repair_proxy(&self, proxy: Option<ProxySetting>) -> Result<(), IpcError> {
        let repairing = Arc::clone(&self.proxy_repair).lock_owned().await;
        if self.cancel_startup.is_cancelled() {
            return Err(IpcError::internal("Jelly is shutting down."));
        }
        if !matches!(*self.phase.borrow(), Phase::Failed { can_repair_proxy: true, .. }) {
            return Err(IpcError::invalid_input("Proxy repair is not needed."));
        }
        HttpClient::new(proxy.as_ref()).map_err(|error| proxy_refused(&error))?;
        let deps = self.deps.clone();
        let cancel = self.cancel_startup.clone();
        self.startup
            .spawn(async move {
                let _repairing = repairing;
                let started = WalletEngine::start(
                    EngineConfig::default(),
                    deps.engine_deps(Arc::new(AdapterFactory::new())),
                )
                .await
                .map_err(|failure| IpcError::from(failure.error))?;
                let saved = if cancel.is_cancelled() {
                    Err(IpcError::internal("Jelly is shutting down."))
                } else {
                    started.engine.set_proxy(proxy).await.map_err(IpcError::from)
                };
                let stopped = started.engine.shutdown().await;
                saved?;
                stopped?;
                Ok(())
            })
            .await?
    }

    pub(crate) fn spawn_startup<F>(&self, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = self.cancel_startup.clone();
        // Tauri's `setup` runs outside the runtime, where `tokio::spawn` would panic.
        async_runtime::spawn(self.startup.track_future(cancel.run_until_cancelled_owned(task)));
    }

    /// Stops startup and drains admitted repairs before services or the runtime can exit.
    pub(crate) async fn stop_startup(&self) {
        self.cancel_startup.cancel();
        let _repairing = self.proxy_repair.lock().await;
        self.startup.close();
        self.startup.wait().await;
    }

    /// Publishes startup's outcome and wakes waiting commands.
    pub fn finish(&self, outcome: Result<Arc<Services>, StartFailure>) {
        let phase = match outcome {
            Ok(services) => Phase::Ready(services),
            Err(StartFailure { error, rebuilt }) => {
                tracing::error!(target: TARGET, %error, "jelly couldn't start");
                let can_repair_proxy = matches!(error, StartupError::NetworkSettings { .. });
                Phase::Failed { error: IpcError::from(error), can_repair_proxy, rebuilt }
            }
        };
        if self.phase.borrow().is_terminal() {
            tracing::warn!(target: TARGET, "startup ran twice");
        }
        self.phase.send_replace(phase);
    }
}
