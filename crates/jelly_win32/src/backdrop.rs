use std::ptr::without_provenance_mut;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Foundation::TypedEventHandler;
use windows::System::{DispatcherQueue, DispatcherQueueController};
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{CompositionColorBrush, Compositor, SpriteVisual};
use windows::UI::ViewManagement::UISettings;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{DWMWA_USE_HOSTBACKDROPBRUSH, DwmSetWindowAttribute};
use windows::Win32::System::WinRT::Composition::ICompositorDesktopInterop;
use windows::Win32::System::WinRT::{
    CreateDispatcherQueueController, DQTAT_COM_NONE, DQTYPE_THREAD_CURRENT, DispatcherQueueOptions,
};
use windows::core::{BOOL, Error as WindowsError, IInspectable, Interface as _, Ref};
use windows_numerics::Vector2;

use crate::error::Win32Error;

/// Windows 11 reports major version 10 like Windows 10, which can't blur behind a window,
/// so only its first build number tells the two apart.
const FIRST_BACKDROP_BUILD: u32 = 22_000;

pub fn is_backdrop_supported() -> bool {
    windows_version::OsVersion::current().build >= FIRST_BACKDROP_BUILD
}

/// A tint drawn behind the window's children, over a blur of the screen behind the window
/// while the layer is blurred.
///
/// The window manager samples and blurs the screen itself, as it does for the Windows 11
/// acrylic material, so the app never sees those pixels. The app draws this layer, not the
/// window frame, so it decides what the page sits on while the frame's shadow and border
/// still follow focus. The layer lasts as long as this value.
///
/// Create it and call its methods on the thread that owns the window, since Windows won't
/// draw into a window from any other thread.
#[derive(Debug)]
pub struct BackdropLayer {
    blur_visual: SpriteVisual,
    tint_brush: CompositionColorBrush,
    tint: Tint,
    _target: DesktopWindowTarget,
    _queue_controller: Option<DispatcherQueueController>,
}

impl BackdropLayer {
    /// A solid layer paints the tint opaque with nothing under it.
    pub fn set_blurred(&self, is_blurred: bool) -> Result<(), Win32Error> {
        let tint_alpha = if is_blurred { self.tint.alpha } else { 0xff };
        self.blur_visual.SetIsVisible(is_blurred)?;
        self.tint_brush.SetColor(Color::from(Tint { alpha: tint_alpha, ..self.tint }))?;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Tint {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    pub alpha: u8,
}

impl From<Tint> for Color {
    fn from(Tint { red, green, blue, alpha }: Tint) -> Self {
        Self { A: alpha, R: red, G: green, B: blue }
    }
}

/// The layer starts out blurred.
#[expect(unsafe_code, reason = "these Win32 calls have no safe wrapper")]
pub fn attach_backdrop(
    window: &impl HasWindowHandle,
    tint: Tint,
) -> Result<BackdropLayer, Win32Error> {
    let window_handle = window.window_handle()?;
    let RawWindowHandle::Win32(handle) = window_handle.as_raw() else {
        return Err(Win32Error::NotWin32);
    };
    let hwnd = HWND(without_provenance_mut(handle.hwnd.get().cast_unsigned()));

    // The window manager samples the screen for a window only once it has opted in.
    let is_allowed = BOOL::from(true);
    let attribute_bytes = u32::try_from(size_of::<BOOL>()).map_err(WindowsError::from)?;
    // SAFETY: the pointer and size describe one BOOL that outlives the call, and
    // `window_handle` is held, so `hwnd` is a live window for this call.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_HOSTBACKDROPBRUSH,
            (&raw const is_allowed).cast(),
            attribute_bytes,
        )
    }?;

    // A compositor needs a dispatcher queue on its thread, and a thread holds at most one.
    let queue_controller = if DispatcherQueue::GetForCurrentThread().is_ok() {
        None
    } else {
        let options_bytes =
            u32::try_from(size_of::<DispatcherQueueOptions>()).map_err(WindowsError::from)?;
        let options = DispatcherQueueOptions {
            dwSize: options_bytes,
            threadType: DQTYPE_THREAD_CURRENT,
            apartmentType: DQTAT_COM_NONE,
        };
        // SAFETY: the options are fully initialized and describe the calling thread.
        Some(unsafe { CreateDispatcherQueueController(options) }?)
    };

    let compositor = Compositor::new()?;
    let root = compositor.CreateContainerVisual()?;
    root.SetRelativeSizeAdjustment(Vector2::one())?;
    let children = root.Children()?;
    let blur_visual = compositor.CreateSpriteVisual()?;
    blur_visual.SetBrush(&compositor.CreateHostBackdropBrush()?)?;
    blur_visual.SetRelativeSizeAdjustment(Vector2::one())?;
    children.InsertAtTop(&blur_visual)?;
    let tint_brush = compositor.CreateColorBrush()?;
    let tint_visual = compositor.CreateSpriteVisual()?;
    tint_visual.SetBrush(&tint_brush)?;
    tint_visual.SetRelativeSizeAdjustment(Vector2::one())?;
    children.InsertAtTop(&tint_visual)?;

    let interop: ICompositorDesktopInterop = compositor.cast()?;
    let is_topmost = false;
    // SAFETY: `window_handle` is held, so `hwnd` is a live window for this call.
    let target = unsafe { interop.CreateDesktopWindowTarget(hwnd, is_topmost) }?;
    target.SetRoot(&root)?;
    let layer = BackdropLayer {
        blur_visual,
        tint_brush,
        tint,
        _target: target,
        _queue_controller: queue_controller,
    };
    layer.set_blurred(true)?;
    Ok(layer)
}

/// A subscription to the transparency effects switch in Windows settings, which battery
/// saver also turns off. Dropping it ends the subscription.
#[derive(Debug)]
pub struct EffectsWatch {
    settings: UISettings,
    token: i64,
}

impl Drop for EffectsWatch {
    fn drop(&mut self) {
        let _ = self.settings.RemoveAdvancedEffectsEnabledChanged(self.token);
    }
}

/// Reports whether transparency effects are on now, and calls `on_change` from a system
/// thread each time the switch moves.
pub fn watch_effects(
    on_change: impl Fn(bool) + Send + 'static,
) -> Result<(bool, EffectsWatch), Win32Error> {
    let settings = UISettings::new()?;
    let handler =
        TypedEventHandler::new(move |sender: Ref<'_, UISettings>, _args: Ref<'_, IInspectable>| {
            if let Some(settings) = sender.as_ref() {
                on_change(settings.AdvancedEffectsEnabled()?);
            }
            Ok(())
        });
    let token = settings.AdvancedEffectsEnabledChanged(&handler)?;
    let watch = EffectsWatch { settings, token };
    let is_enabled = watch.settings.AdvancedEffectsEnabled()?;
    Ok((is_enabled, watch))
}
