use tauri::{App, AppHandle, Manager as _, async_runtime};

use crate::error::AppError;
use crate::services::{Services, ShellConfig, ShellDeps};
use crate::state::AppState;
use crate::{event, log, window};

const TARGET: &str = "jelly::app::startup";

/// Under the platform's local, non-roaming application data directory. The database and
/// its write-ahead log must stay off a roaming profile.
const DATA_DIR: &str = "Jelly";

/// Starts services in the background so the window can open first. Engine commands wait for
/// startup.
pub(crate) fn install(app: &App) -> Result<(), AppError> {
    let data_dir = app
        .path()
        .local_data_dir()
        .map_err(|source| AppError::DataDirectory { source })?
        .join(DATA_DIR);
    log::install(&data_dir);
    tracing::info!(target: TARGET, "jelly is starting");

    let deps = ShellDeps::platform(data_dir);
    app.manage(AppState::new(deps.clone()));
    let state = app.state::<AppState>();
    let handle = app.handle().clone();
    let pump = event::startup_pump(handle.clone(), state.phases());

    // The pump ends at the phase `finish` publishes, so joining the two ends the task.
    state.spawn_startup(async move {
        let start = async {
            let outcome = Services::start(ShellConfig::default(), deps).await;
            if let Ok(services) = &outcome
                && let Ok(engine) = services.engine()
            {
                window::apply_backdrop(&handle, engine.app_settings().await.backdrop);
            }
            let state = handle.state::<AppState>();
            state.finish(outcome);
            if let Some(services) = state.services_now() {
                event::spawn_pumps(&handle, &services);
            }
        };
        tokio::join!(pump, start);
    });
    Ok(())
}

/// Stops services on the exit event while the runtime still exists. A startup still
/// running is stopped first, so it can't leave services running after exit.
pub(crate) fn shutdown(handle: &AppHandle) {
    let state = handle.state::<AppState>();
    async_runtime::block_on(async {
        state.stop_startup().await;
        if let Some(services) = state.services_now() {
            services.shutdown().await;
        }
    });
    tracing::info!(target: TARGET, "jelly stopped");
}
