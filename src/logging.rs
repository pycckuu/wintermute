//! Structured logging setup using `tracing-subscriber` and `tracing-appender`.
//!
//! Two modes:
//! - **Production** ([`init_production`]): JSON file layer (daily rotation) + console layer
//! - **CLI** ([`init_cli`]): console-only for one-shot subcommands

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Holds the non-blocking writer guard for file logging.
///
/// The [`WorkerGuard`] must be kept alive for the duration of the process.
/// Dropping it flushes pending log entries and closes the file.
pub struct LoggingGuard {
    _guard: WorkerGuard,
}

/// Initialise logging for the `start` subcommand (production mode).
///
/// Writes JSON logs to `{logs_dir}/wintermute.log.YYYY-MM-DD` with daily
/// rotation. Also emits human-readable output to stderr controlled by the
/// `RUST_LOG` environment variable (default: `info`).
///
/// Returns a [`LoggingGuard`] that must be kept alive for log flushing.
///
/// # Errors
///
/// Returns an error if the logs directory cannot be created.
pub fn init_production(logs_dir: &Path) -> anyhow::Result<LoggingGuard> {
    std::fs::create_dir_all(logs_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to create logs directory {}: {e}",
            logs_dir.display()
        )
    })?;

    let file_appender = tracing_appender::rolling::daily(logs_dir, "wintermute.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(non_blocking);

    let console_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(env_filter)
        .with(json_layer)
        .with(console_layer)
        .init();

    Ok(LoggingGuard { _guard: guard })
}

/// Initialise minimal logging for non-`start` subcommands (CLI mode).
///
/// Emits human-readable output to stderr only. No file rotation.
/// Controlled by `RUST_LOG` (default: `info`).
pub fn init_cli() {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_writer(std::io::stderr)
        .init();
}
