use tauri::{Manager as _, RunEvent, Theme, async_runtime};
use tauri_plugin_prevent_default::Flags;
use tokio::runtime::Builder as RuntimeBuilder;

use crate::error::AppError;
use crate::{log, startup, surface, window};

const TARGET: &str = "jelly::app";

/// Bounds concurrent password derivations, each of which can allocate 256 MiB.
const BLOCKING_THREADS: usize = 16;

/// Runs the desktop event loop and shuts down the wallet before its runtime exits.
pub fn run() -> Result<(), AppError> {
    log::install_panic_hook();
    let runtime = RuntimeBuilder::new_multi_thread()
        .enable_all()
        .max_blocking_threads(BLOCKING_THREADS)
        .build()
        .map_err(|source| AppError::Runtime { source })?;
    async_runtime::set(runtime.handle().clone());

    // Shift+Tab stays with the page, which needs it to move focus backward.
    let prevent_default = tauri_plugin_prevent_default::Builder::new()
        .with_flags(Flags::debug().difference(Flags::FOCUS_MOVE));
    #[cfg(all(windows, not(debug_assertions)))]
    let prevent_default = prevent_default.platform(
        tauri_plugin_prevent_default::PlatformOptions::new()
            .browser_accelerator_keys(false)
            .built_in_error_page(false)
            .default_context_menus(false)
            .default_script_dialogs(false)
            .dev_tools(false)
            .general_autofill(false)
            .password_autosave(false)
            .pinch_zoom(false)
            .swipe_navigation(false)
            .zoom_control(false),
    );

    let ipc = surface::builder();
    let context = tauri::generate_context!();
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|handle, _args, _cwd| {
            if let Some(window) = handle.get_webview_window(window::MAIN_WINDOW) {
                if let Err(error) = window.unminimize() {
                    tracing::warn!(target: TARGET, %error, "the window could not be restored");
                }
                if let Err(error) = window.set_focus() {
                    tracing::warn!(target: TARGET, %error, "the window could not take focus");
                }
            }
        }))
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(prevent_default.build())
        .invoke_handler(ipc.invoke_handler())
        .setup(move |app| {
            ipc.mount_events(app);
            app.set_theme(Some(Theme::Dark));
            startup::install(app)?;
            window::open_main(app)?;
            Ok(())
        })
        .build(context)?;

    app.run(|handle, event| {
        if matches!(event, RunEvent::Exit) {
            startup::shutdown(handle);
        }
    });
    Ok(())
}
