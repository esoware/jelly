//! Desktop entry point, IPC surface and wallet service lifecycle.

#![deny(clippy::float_arithmetic)]
mod command;
mod endpoints;
mod error;
mod event;
mod log;
mod services;
mod shell;
mod startup;
mod state;
mod surface;
#[cfg(feature = "test-util")]
pub mod test_util;
mod window;

pub use crate::error::{AppError, IpcError, IpcErrorCode, IpcErrorDetail, ShellErrorCode};
pub use crate::services::{Services, ShellConfig, ShellDeps};
pub use crate::shell::run;
pub use crate::state::{AppState, StartupStatus};
