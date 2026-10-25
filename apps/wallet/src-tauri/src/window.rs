#[cfg(windows)]
mod backdrop;

#[cfg(windows)]
use std::time::Duration;

use jelly_core::wire_enum;
use specta::Type;
#[cfg(target_os = "macos")]
use tauri::TitleBarStyle;
#[cfg(windows)]
use tauri::WebviewWindow;
use tauri::webview::{Color, DownloadEvent, NewWindowResponse};
use tauri::{App, Manager as _, Url, WebviewWindowBuilder};

use crate::error::AppError;
#[cfg(windows)]
pub(crate) use backdrop::apply as apply_backdrop;

const TARGET: &str = "jelly::app::window";

pub(crate) const MAIN_WINDOW: &str = "main";

// A window without a backdrop layer shows `--color-canvas` from `styles/tokens.css`
// before the page paints and wherever a resize runs ahead of the page.
const CANVAS: Color = Color(0x18, 0x18, 0x18, 0xff);

/// How soon after a crashed page is reloaded another crash leaves it blank instead, so a
/// page that crashes while it loads is not reloaded forever.
#[cfg(windows)]
const CRASH_RELOAD_COOLDOWN: Duration = Duration::from_secs(10);

wire_enum! {
    #[derive(Type)]
    /// What fills the window behind the page.
    pub(crate) enum WindowBackdrop {
        /// The window is transparent over a layer the app draws, either a blur of the
        /// screen behind the window under a tint or the plain canvas, so the page leaves
        /// its canvas unpainted.
        Composed => "composed",
        /// The window paints the canvas, and so does the page.
        Solid => "solid",
    }
}

wire_enum! {
    #[derive(Type)]
    /// Who draws the buttons that minimize, maximize, and close the window.
    pub(crate) enum WindowControls {
        /// The page draws them at the right end of its title bar.
        Page => "page",
        /// The system draws them over the page's title bar.
        System => "system",
    }
}

/// macOS users expect the system's buttons at the top left. Elsewhere the window has no
/// frame, so the page draws its own.
pub(crate) const WINDOW_CONTROLS: WindowControls =
    if cfg!(target_os = "macos") { WindowControls::System } else { WindowControls::Page };

/// Builds the main window in code, so it can refuse navigation, new windows, and downloads.
///
/// The content security policy stops fetches and subresources, but not a top-level
/// navigation, `window.open`, or a download. The handlers attached here refuse all three,
/// so a script that gets into the page still has no way to reach the network or the disk.
pub(crate) fn open_main(app: &App) -> Result<(), AppError> {
    let config = app.config();
    let window_config = config
        .app
        .windows
        .iter()
        .find(|window_config| window_config.label == MAIN_WINDOW)
        .ok_or(AppError::WindowConfig)?;
    let dev_url = if tauri::is_dev() { config.build.dev_url.clone() } else { None };

    // Hidden until the backdrop or solid canvas covers the first frame.
    let builder = WebviewWindowBuilder::from_config(app, window_config)?
        .visible(false)
        .on_navigation(move |url| {
            let is_allowed = is_app_origin(url, dev_url.as_ref());
            if !is_allowed {
                tracing::warn!(target: TARGET, "the page tried to leave the app and was stopped");
            }
            is_allowed
        })
        .on_new_window(|_, _| NewWindowResponse::Deny)
        .on_download(|_, event| {
            if matches!(event, DownloadEvent::Requested { .. }) {
                tracing::warn!(target: TARGET, "the page tried to download a file and was stopped");
            }
            false
        });
    // A transparent window shows the desktop wherever nothing paints, so only a window
    // that can get a backdrop layer is made transparent.
    #[cfg(windows)]
    let builder = builder.transparent(jelly_win32::is_backdrop_supported());
    #[cfg(target_os = "macos")]
    let builder =
        builder.decorations(true).title_bar_style(TitleBarStyle::Overlay).hidden_title(true);
    let window = builder.build()?;
    #[cfg(windows)]
    reload_after_crash(&window)?;
    // A debug build keeps the browser keys and menu for reloading and inspecting the page.
    #[cfg(all(windows, not(debug_assertions)))]
    window.with_webview(|webview| {
        if let Err(error) = jelly_win32::lock_down_webview(&webview.controller()) {
            tracing::warn!(target: TARGET, %error, "the webview kept its browser keys and context menu");
        }
    })?;

    #[cfg(windows)]
    let backdrop = backdrop::attach(app, &window);
    #[cfg(not(windows))]
    let backdrop = WindowBackdrop::Solid;
    if backdrop == WindowBackdrop::Solid {
        window.set_background_color(Some(CANVAS))?;
    }
    app.manage(backdrop);
    window.show()?;
    Ok(())
}

/// A page whose renderer exits stays blank until something reloads it.
#[cfg(windows)]
fn reload_after_crash(window: &WebviewWindow) -> Result<(), AppError> {
    use jelly_core::{Clock as _, Deadline, SystemClock};

    let reloader = window.clone();
    window.with_webview(move |webview| {
        let mut reload_allowed_at = Deadline::EXPIRED;
        let watched = jelly_win32::on_renderer_exit(&webview.controller(), move || {
            if !reload_allowed_at.is_expired(&SystemClock) {
                tracing::error!(target: TARGET, "the page crashed again right after a reload and was left blank");
                return;
            }
            reload_allowed_at = SystemClock.deadline_in(CRASH_RELOAD_COOLDOWN);
            match reloader.reload() {
                Ok(()) => tracing::warn!(target: TARGET, "the page crashed and was reloaded"),
                Err(error) => {
                    tracing::warn!(target: TARGET, %error, "the page crashed and could not be reloaded");
                }
            }
        });
        if let Err(error) = watched {
            tracing::warn!(target: TARGET, %error, "the page will stay blank if it crashes");
        }
    })?;
    Ok(())
}

#[cfg(not(windows))]
pub(crate) const fn apply_backdrop(
    _handle: &tauri::AppHandle,
    _setting: jelly_engine::BackdropSetting,
) {
}

/// Whether a navigation to `url` stays inside the app.
///
/// Production pages load from `tauri://localhost`, or on Windows from
/// `http://tauri.localhost`, since the window leaves `useHttpsScheme` off. The webview
/// serves no other scheme on those hosts, and `tauri.localhost` on a scheme it does not
/// serve is a loopback socket. Development pages load from the Vite server.
fn is_app_origin(url: &Url, dev_url: Option<&Url>) -> bool {
    match url.scheme() {
        "tauri" => !cfg!(windows) && url.host_str() == Some("localhost") && url.port().is_none(),
        "http" if cfg!(windows) && url.host_str() == Some("tauri.localhost") => {
            url.port_or_known_default() == Some(80)
        }
        "http" | "https" => dev_url.is_some_and(|dev| dev.origin() == url.origin()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).expect("a valid url")
    }

    #[test]
    fn the_app_origins_are_allowed() {
        let origin = if cfg!(windows) { "http://tauri.localhost" } else { "tauri://localhost" };
        for path in ["/", "/wallet/send?chain=bitcoin", "/settings"] {
            let allowed = format!("{origin}{path}");
            assert!(is_app_origin(&url(&allowed), None), "{allowed} is the app");
        }
    }

    #[test]
    fn the_dev_server_is_allowed_only_when_configured() {
        let dev_url = url("http://localhost:1420");
        assert!(is_app_origin(&url("http://localhost:1420/unlock"), Some(&dev_url)));
        assert!(!is_app_origin(&url("http://localhost:1420/unlock"), None));
        assert!(!is_app_origin(&url("http://localhost:1421/"), Some(&dev_url)), "another port");
        assert!(!is_app_origin(&url("https://localhost:1420/"), Some(&dev_url)), "another scheme");
    }

    #[test]
    fn everything_else_is_refused() {
        let dev_url = url("http://localhost:1420");
        let other_platform =
            if cfg!(windows) { "tauri://localhost/" } else { "http://tauri.localhost/" };
        for refused in [
            other_platform,
            "https://example.com/?seed=abandon",
            "http://tauri.localhost.example.com/",
            "http://tauri.localhost:1420/",
            "http://tauri.localhost:81/",
            "tauri://localhost:1420/",
            "http://localhost/",
            "tauri://example.com/",
            "file:///C:/Windows/win.ini",
            "about:blank",
            "data:text/html,hello",
            "javascript:alert(1)",
            "ipc://localhost/",
        ] {
            assert!(!is_app_origin(&url(refused), Some(&dev_url)), "{refused} is not the app");
        }
    }

    #[test]
    fn the_app_host_on_a_scheme_the_webview_does_not_serve_is_refused() {
        let unserved = if cfg!(windows) {
            "https://tauri.localhost/?seed=abandon"
        } else {
            "http://tauri.localhost/?seed=abandon"
        };
        assert!(
            !is_app_origin(&url(unserved), None),
            "{unserved} is a loopback socket the webview does not serve, not the app"
        );
    }
}
