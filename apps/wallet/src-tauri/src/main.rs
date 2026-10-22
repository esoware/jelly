#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    #[cfg(windows)]
    if let Some(code) = relaunch_without_webview2_overrides() {
        return code;
    }
    match jelly_lib::run() {
        Ok(()) => ExitCode::SUCCESS,
        // This reaches the log file only if the start got far enough to install the
        // subscriber, which needs the data directory.
        Err(error) => {
            tracing::error!(target: "jelly::app", %error, "jelly could not run");
            ExitCode::FAILURE
        }
    }
}

/// Runs a copy of Jelly without any `WEBVIEW2_*` environment variable and waits for it.
///
/// The Windows webview reads those variables when it starts, and any program running as the
/// user can set them to open a remote debugging port into the page or to load a different
/// browser runtime. Removing a variable from this process takes `unsafe`, so Jelly starts a
/// copy of itself without them instead.
#[cfg(windows)]
fn relaunch_without_webview2_overrides() -> Option<ExitCode> {
    use std::env;
    use std::ffi::OsString;
    use std::process::Command;

    let overrides: Vec<OsString> = env::vars_os()
        .map(|(name, _)| name)
        .filter(|name| name.to_string_lossy().to_ascii_uppercase().starts_with("WEBVIEW2_"))
        .collect();
    if overrides.is_empty() {
        return None;
    }
    let status = env::current_exe().and_then(|executable| {
        let mut relaunch = Command::new(executable);
        relaunch.args(env::args_os().skip(1));
        for name in &overrides {
            relaunch.env_remove(name);
        }
        relaunch.status()
    });
    let is_success = status.is_ok_and(|exit| exit.success());
    Some(if is_success { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
