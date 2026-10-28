use std::backtrace::Backtrace;
use std::fs;
use std::io;
use std::panic::{self, PanicHookInfo};
use std::path::Path;
use std::sync::OnceLock;
use std::thread;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::BoxMakeWriter;

const TARGET: &str = "jelly::app::log";

const LOG_DIR: &str = "logs";
const LOG_PREFIX: &str = "jelly.log";
const FILTER_ENV: &str = "JELLY_LOG";

/// Jelly's own targets at info and everything else at warn. Every target in the workspace
/// starts with `jelly`, so one directive covers them all.
const DEFAULT_FILTER: &str = "warn,jelly=info";

const KEEP_DAYS: usize = 14;

/// Kept for the life of the process. The non-blocking writer's worker stops when its guard
/// drops, and any buffered lines are lost.
static GUARD: OnceLock<WorkerGuard> = OnceLock::new();

/// Logs panics from every thread, background ones included.
pub(crate) fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let thread = thread::current();
        let location = info.location().map_or_else(String::new, ToString::to_string);
        tracing::error!(
            target: "jelly::app",
            thread = thread.name().unwrap_or("unnamed"),
            location = %location,
            payload = %panic_payload(info),
            backtrace = %Backtrace::capture(),
            "a thread panicked"
        );
        previous(info);
    }));
}

fn panic_payload<'a>(info: &'a PanicHookInfo<'a>) -> &'a str {
    let payload = info.payload();
    if let Some(message) = payload.downcast_ref::<&str>() {
        return message;
    }
    payload.downcast_ref::<String>().map_or("", String::as_str)
}

/// Installs the process-wide subscriber.
///
/// Infallible on purpose, because a wallet that can't write a log file still has to open
/// and report the unwritable data directory in its window. Without a log directory, lines
/// go to stderr in a debug build and nowhere in a release build.
pub(crate) fn install(data_dir: &Path) {
    let directory = data_dir.join(LOG_DIR);
    let (writer, failure) = match fs::create_dir_all(&directory) {
        Ok(()) => (file_writer(&directory), None),
        Err(error) => (BoxMakeWriter::new(io::sink), Some(error)),
    };

    #[cfg(debug_assertions)]
    let writer = {
        use tracing_subscriber::fmt::writer::MakeWriterExt as _;
        BoxMakeWriter::new(writer.and(io::stderr))
    };

    let filter =
        EnvFilter::try_from_env(FILTER_ENV).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let is_installed = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(filter)
        .with_writer(writer)
        .try_init()
        .is_ok();
    if !is_installed {
        // Another subscriber is already installed, and it stays.
        return;
    }

    // Only a debug build's stderr can carry this.
    if let Some(error) = failure {
        tracing::warn!(
            target: TARGET,
            %error,
            "the log directory couldn't be created; this session isn't being logged"
        );
    }
}

/// A daily rolling file, or nowhere if the file can't be opened or the appender's guard
/// can't be parked.
fn file_writer(directory: &Path) -> BoxMakeWriter {
    let Ok(appender) = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_PREFIX)
        .max_log_files(KEEP_DAYS)
        .build(directory)
    else {
        return BoxMakeWriter::new(io::sink);
    };
    let (writer, guard) = tracing_appender::non_blocking(appender);
    if GUARD.set(guard).is_err() {
        return BoxMakeWriter::new(io::sink);
    }
    BoxMakeWriter::new(writer)
}
