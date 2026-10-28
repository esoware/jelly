use webview2_com::Microsoft::Web::WebView2::Win32::{
    COREWEBVIEW2_PROCESS_FAILED_KIND, COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
    ICoreWebView2_11, ICoreWebView2AcceleratorKeyPressedEventArgs2, ICoreWebView2Controller,
};
use webview2_com::{
    AcceleratorKeyPressedEventHandler, ContextMenuRequestedEventHandler, ProcessFailedEventHandler,
};
use windows::core::Interface as _;

use crate::error::Win32Error;

/// Keeps the browser from acting on its own keys, such as reload, print, find, zoom,
/// history, and the developer tools, and from opening its own context menu. The page still
/// receives every key.
///
/// The equivalent webview settings apply only from the next page load, and a runtime update
/// has broken the key setting before. These handlers run ahead of the browser for every
/// accelerator key and context menu, whatever the settings say.
#[expect(unsafe_code, reason = "the webview's COM interfaces have no safe wrapper")]
pub fn lock_down_webview(controller: &ICoreWebView2Controller) -> Result<(), Win32Error> {
    // The handlers last as long as the webview, so their tokens are never needed.
    let mut token = 0;

    let key_handler = AcceleratorKeyPressedEventHandler::create(Box::new(|_, args| {
        if let Some(args) = args {
            let args: ICoreWebView2AcceleratorKeyPressedEventArgs2 = args.cast()?;
            // SAFETY: the arguments are live for the length of the handler call.
            unsafe { args.SetIsBrowserAcceleratorKeyEnabled(false) }?;
        }
        Ok(())
    }));
    // SAFETY: the controller is live, and the token is a valid place for the call to write.
    unsafe { controller.add_AcceleratorKeyPressed(&key_handler, &raw mut token) }?;

    // SAFETY: the controller is live.
    let webview: ICoreWebView2_11 = unsafe { controller.CoreWebView2() }?.cast()?;
    let menu_handler = ContextMenuRequestedEventHandler::create(Box::new(|_, args| {
        if let Some(args) = args {
            // SAFETY: the arguments are live for the length of the handler call.
            unsafe { args.SetHandled(true) }?;
        }
        Ok(())
    }));
    // SAFETY: the webview is live, and the token is a valid place for the call to write.
    unsafe { webview.add_ContextMenuRequested(&menu_handler, &raw mut token) }?;
    Ok(())
}

/// Calls `on_exit` each time the process rendering the page exits, after which the page is
/// gone until it's reloaded.
#[expect(unsafe_code, reason = "the webview's COM interfaces have no safe wrapper")]
pub fn on_renderer_exit(
    controller: &ICoreWebView2Controller,
    mut on_exit: impl FnMut() + 'static,
) -> Result<(), Win32Error> {
    // SAFETY: the controller is live.
    let webview = unsafe { controller.CoreWebView2() }?;
    let handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
        if let Some(args) = args {
            let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
            // SAFETY: the arguments are live for the length of the handler call, and `kind`
            // is a valid place for the call to write.
            unsafe { args.ProcessFailedKind(&raw mut kind) }?;
            if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
                on_exit();
            }
        }
        Ok(())
    }));
    // The handler lasts as long as the webview, so its token is never needed.
    let mut token = 0;
    // SAFETY: the webview is live, and the token is a valid place for the call to write.
    unsafe { webview.add_ProcessFailed(&handler, &raw mut token) }?;
    Ok(())
}
