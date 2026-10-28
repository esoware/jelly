use jelly_engine::{AppSettings, BackdropSetting};
use jelly_win32::{BackdropLayer, EffectsWatch, Tint};
use parking_lot::Mutex;
use tauri::{App, AppHandle, Manager as _, WebviewWindow};

use super::{CANVAS, TARGET, WindowBackdrop};

// The canvas at 85% over the blur, no heavier than the page's veil over its own blurred
// popups, since a whole window of blur reads fainter than a popup's. The layer applies it,
// not the page, so the strip a resize uncovers before the page catches up is tinted too.
const BACKDROP_TINT: Tint = Tint { red: CANVAS.0, green: CANVAS.1, blue: CANVAS.2, alpha: 0xd9 };

/// Blurs only while the setting asks for it and transparency effects are on, whether or
/// not the window is active.
struct Backdrop {
    layer: BackdropLayer,
    setting: BackdropSetting,
    has_effects: bool,
    _effects: EffectsWatch,
}

impl Backdrop {
    fn paint(&self) {
        let is_blurred = self.setting == BackdropSetting::Blur && self.has_effects;
        if let Err(error) = self.layer.set_blurred(is_blurred) {
            tracing::warn!(target: TARGET, %error, "the window backdrop wasn't repainted");
        }
    }
}

/// Starts from the shipped default setting, and the stored one follows once the engine has
/// read it.
pub(super) fn attach(app: &App, window: &WebviewWindow) -> WindowBackdrop {
    if !jelly_win32::is_backdrop_supported() {
        return WindowBackdrop::Solid;
    }
    let handle = app.handle().clone();
    let (has_effects, effects) = match jelly_win32::watch_effects(move |has_effects| {
        update_backdrop(&handle, move |backdrop| backdrop.has_effects = has_effects);
    }) {
        Ok(watched) => watched,
        Err(error) => {
            tracing::warn!(target: TARGET, %error, "the transparency setting couldn't be read");
            return WindowBackdrop::Solid;
        }
    };
    let layer = match jelly_win32::attach_backdrop(window, BACKDROP_TINT) {
        Ok(layer) => layer,
        Err(error) => {
            tracing::warn!(target: TARGET, %error, "the window couldn't take a backdrop layer");
            return WindowBackdrop::Solid;
        }
    };
    let backdrop = Backdrop {
        layer,
        setting: AppSettings::default().backdrop,
        has_effects,
        _effects: effects,
    };
    backdrop.paint();
    app.manage(Mutex::new(backdrop));

    WindowBackdrop::Composed
}

/// Windows refuses to draw into a window from any thread but the one that created it, so
/// the change runs on the main thread. A window without a layer has nothing to change.
fn update_backdrop(handle: &AppHandle, change: impl FnOnce(&mut Backdrop) + Send + 'static) {
    let main_thread_handle = handle.clone();
    let queued = handle.run_on_main_thread(move || {
        if let Some(state) = main_thread_handle.try_state::<Mutex<Backdrop>>() {
            let mut backdrop = state.lock();
            change(&mut backdrop);
            backdrop.paint();
        }
    });
    if let Err(error) = queued {
        tracing::warn!(target: TARGET, %error, "the window backdrop change wasn't queued");
    }
}

pub(crate) fn apply(handle: &AppHandle, setting: BackdropSetting) {
    update_backdrop(handle, move |backdrop| backdrop.setting = setting);
}
