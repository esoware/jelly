//! The Windows calls the shell makes for its window and webview. Most of them need
//! `unsafe`, which no other crate may use.

#![cfg(windows)]
#![deny(clippy::float_arithmetic)]

mod backdrop;
mod error;
mod webview;

pub use crate::backdrop::{
    BackdropLayer, EffectsWatch, Tint, attach_backdrop, is_backdrop_supported, watch_effects,
};
pub use crate::error::Win32Error;
pub use crate::webview::{lock_down_webview, on_renderer_exit};
