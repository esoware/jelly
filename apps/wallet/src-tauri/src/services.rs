use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use jelly_chains::AdapterFactory;
use jelly_core::{
    AdapterBuilder, AssetId, Clock, CoreError, FiatCurrency, ProxySetting, SystemClock,
};
use jelly_engine::{
    DatabaseRebuilt, EngineConfig, EngineDeps, StartFailure, Started, StartupError, WalletEngine,
};
use jelly_net::{NetError, Network};
use jelly_prices::{
    CandleInterval, Fetch, PriceChart, PriceCharts, PriceFeed, PriceFeedConfig, PriceHistoryFeed,
};
use jelly_vault::{KeyringStore, MasterKeyStore};
use parking_lot::Mutex;
use tokio::sync::{Mutex as AsyncMutex, watch};
use tokio::task::spawn_blocking;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::endpoints::{load_endpoint_overrides, persist_endpoint_health, restore_endpoint_health};
use crate::error::IpcError;

const TARGET: &str = "jelly::app::services";

#[derive(Debug, Default)]
pub struct ShellConfig {
    pub engine: EngineConfig,
    pub prices: PriceFeedConfig,
}

/// Lets startup tests inject the clock and credential store. A test that must stay offline
/// also asks the price feed in [`ShellConfig`] for no tickers.
#[derive(Debug, Clone)]
pub struct ShellDeps {
    pub data_dir: PathBuf,
    pub clock: Arc<dyn Clock>,
    pub keystore: Arc<dyn MasterKeyStore>,
}

impl ShellDeps {
    pub(crate) fn platform(data_dir: PathBuf) -> Self {
        Self { data_dir, clock: Arc::new(SystemClock), keystore: Arc::new(KeyringStore) }
    }

    pub(crate) fn engine_deps(self, builder: Arc<dyn AdapterBuilder>) -> EngineDeps {
        let Self { data_dir, clock, keystore } = self;
        EngineDeps { data_dir, clock, keystore, builder, table: jelly_chains::chains() }
    }
}

pub struct Services {
    /// Engine shutdown needs the last engine handle to checkpoint the write-ahead log, so
    /// the one here is taken before it.
    engine: Mutex<Option<WalletEngine>>,
    network: Arc<Network>,
    proxy_write: AsyncMutex<()>,
    /// Held from a reload's read of the endpoint table to its write to the registry, so a
    /// reload that read before an edit cannot land after the edit's own reload.
    endpoint_reload: AsyncMutex<()>,
    prices: PriceFeed,
    price_history: PriceHistoryFeed,
    price_charts: PriceCharts,
    price_feed_switch: watch::Sender<bool>,
    /// Reports database replacement so the frontend can distinguish it from a first run.
    rebuilt: Option<DatabaseRebuilt>,
    /// Shell tasks cancelled and drained before engine shutdown.
    tasks: TaskTracker,
    cancel: CancellationToken,
}

impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Services")
            .field("running", &self.engine.lock().is_some())
            .field("tasks", &self.tasks.len())
            .finish_non_exhaustive()
    }
}

impl Services {
    /// The engine needs the factory, the network needs the engine's stored TLS pins, and
    /// the factory needs the network, which fixes the order here.
    pub async fn start(config: ShellConfig, deps: ShellDeps) -> Result<Arc<Self>, StartFailure> {
        let clock = Arc::clone(&deps.clock);
        let adapters = Arc::new(AdapterFactory::new());
        // Platform credential stores may block, so the probe runs on the blocking pool.
        let probe = Arc::clone(&deps.keystore);
        spawn_blocking(move || probe.check_available())
            .await
            .map_err(|source| StartupError::Failed {
                context: "checking the credential store",
                source: Box::new(source),
            })
            .and_then(|checked| checked.map_err(StartupError::from))
            .map_err(|error| StartFailure { error, rebuilt: None })?;
        let Started { engine, rebuilt } = WalletEngine::start(
            config.engine,
            deps.engine_deps(Arc::<AdapterFactory>::clone(&adapters)),
        )
        .await?;
        if rebuilt.is_some() {
            tracing::warn!(
                target: TARGET,
                "the previous wallet database could not be used and was replaced"
            );
        }

        let wired = async {
            let settings = engine.app_settings().await;
            let network = Network::start(
                Arc::clone(&clock),
                engine.pin_store(),
                settings.proxy.clone(),
                jelly_chains::chains().entries(),
            )
            .map_err(|source| StartupError::NetworkSettings { source: Box::new(source) })?;
            adapters.attach(Arc::clone(&network));

            load_endpoint_overrides(&engine, network.registry()).await.map_err(unreadable)?;
            restore_endpoint_health(&engine, network.health()).await.map_err(unreadable)?;
            Ok::<_, StartupError>((settings, network))
        }
        .await;
        // A dropped engine skips the lock path and leaves the log to SQLite's own close,
        // which another open connection prevents.
        let (settings, network) = match wired {
            Ok(wired) => wired,
            Err(error) => {
                if let Err(shutdown) = engine.shutdown().await {
                    tracing::warn!(target: TARGET, error = %shutdown, "shutdown did not complete");
                }
                return Err(StartFailure { error, rebuilt });
            }
        };

        let fetch: Arc<dyn Fetch> = Arc::<Network>::clone(&network);
        let (price_history, history_loader) =
            PriceHistoryFeed::new(&config.prices, Arc::clone(&fetch), Arc::clone(&clock));
        let price_charts = PriceCharts::new(&config.prices, Arc::clone(&fetch), Arc::clone(&clock));
        let (prices, poller) = PriceFeed::new(&config.prices, settings.fiat_currency, fetch, clock);
        let (price_feed_switch, enabled) = watch::channel(settings.is_price_feed_enabled);

        let services = Arc::new(Self {
            engine: Mutex::new(Some(engine)),
            network,
            proxy_write: AsyncMutex::new(()),
            endpoint_reload: AsyncMutex::new(()),
            prices,
            price_history,
            price_charts,
            price_feed_switch,
            rebuilt,
            tasks: TaskTracker::new(),
            cancel: CancellationToken::new(),
        });
        services.spawn(history_loader.run(enabled.clone()));
        services.spawn(poller.run(enabled));
        Ok(services)
    }

    pub(crate) const fn rebuilt(&self) -> Option<&DatabaseRebuilt> {
        self.rebuilt.as_ref()
    }

    /// Unavailable after shutdown takes the engine handle.
    pub fn engine(&self) -> Result<WalletEngine, IpcError> {
        self.engine.lock().clone().ok_or_else(|| IpcError::internal("Jelly is shutting down."))
    }

    pub fn network(&self) -> &Network {
        &self.network
    }

    pub const fn prices(&self) -> &PriceFeed {
        &self.prices
    }

    pub(crate) const fn price_history(&self) -> &PriceHistoryFeed {
        &self.price_history
    }

    /// Refused while prices are off, so nothing reaches the provider then.
    pub(crate) async fn price_chart(
        &self,
        asset: AssetId,
        interval: CandleInterval,
    ) -> Result<PriceChart, IpcError> {
        if !*self.price_feed_switch.borrow() {
            return Err(IpcError::prices_unavailable());
        }
        self.price_charts.chart(asset, interval, self.prices.currency()).await.map_err(|error| {
            tracing::warn!(target: TARGET, %error, "a price chart was not loaded");
            IpcError::prices_unavailable()
        })
    }

    /// Saves the setting and pauses or resumes polling without stopping the feed task.
    pub(crate) async fn set_price_feed_enabled(&self, is_enabled: bool) -> Result<(), IpcError> {
        self.engine()?.set_price_feed_enabled(is_enabled).await?;
        self.price_feed_switch.send_replace(is_enabled);
        Ok(())
    }

    /// Saves the setting and reprices from the rates already held, without a request.
    pub(crate) async fn set_fiat_currency(&self, currency: FiatCurrency) -> Result<(), IpcError> {
        self.engine()?.set_fiat_currency(currency).await?;
        self.prices.set_currency(currency);
        Ok(())
    }

    /// Validates without changing egress, persists the setting, then publishes
    /// the prepared route. The price feed follows it even while locked.
    pub async fn set_proxy(&self, proxy: Option<ProxySetting>) -> Result<(), IpcError> {
        let _writing = self.proxy_write.lock().await;
        let prepared =
            self.network.prepare_proxy(proxy.clone()).map_err(|error| proxy_refused(&error))?;
        self.engine()?.set_proxy(proxy).await?;
        self.network.apply_proxy(prepared);
        Ok(())
    }

    /// Reloads endpoint overrides before requesting an adapter rebuild. The endpoint list
    /// must also reflect the edit.
    pub async fn endpoints_changed(&self) -> Result<(), IpcError> {
        let engine = self.engine()?;
        let _reloading = self.endpoint_reload.lock().await;
        load_endpoint_overrides(&engine, self.network.registry()).await?;
        engine.endpoints_changed();
        Ok(())
    }

    /// Registers a shell task that shutdown drops at whatever await it is parked on.
    pub(crate) fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) {
        self.tasks.spawn(self.cancel.clone().run_until_cancelled_owned(task));
    }

    /// Drains shell tasks and persists endpoint health, then shuts the engine down, which
    /// locks the wallet, wipes session keys, stops the pin writer, and checkpoints the
    /// database.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        self.tasks.close();
        self.tasks.wait().await;

        let Some(engine) = self.engine.lock().take() else {
            return;
        };
        if let Err(error) = persist_endpoint_health(&engine, self.network.health()).await {
            tracing::warn!(target: TARGET, %error, "endpoint health was not written");
        }
        if let Err(error) = engine.shutdown().await {
            tracing::warn!(target: TARGET, %error, "shutdown did not complete");
        }
    }
}

fn unreadable(source: CoreError) -> StartupError {
    StartupError::Failed { context: "reading the wallet database", source: Box::new(source) }
}

/// Logs the redacted proxy error and returns a fixed validation message.
pub(crate) fn proxy_refused(error: &NetError) -> IpcError {
    tracing::warn!(target: TARGET, %error, "the outbound route was refused");
    IpcError::invalid_input("That proxy address cannot be used.")
}
