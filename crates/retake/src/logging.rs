//! Logging: a daily rolling file in `%LOCALAPPDATA%\Retake\logs` (7 kept), plus stdout with
//! `--console`.

use std::path::PathBuf;

use anyhow::{Context, Result};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{Builder, Rotation};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

use crate::sysinfo;

/// `%LOCALAPPDATA%\Retake\logs`.
pub fn log_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Retake").join("logs")
}

/// Installs the global subscriber. Keep the guard alive until exit: dropping it flushes.
pub fn init(console: bool) -> Result<WorkerGuard> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let appender = Builder::new()
        .rotation(Rotation::DAILY)
        .filename_prefix("retake")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .context("creating rolling log file")?;
    let (writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_env("RETAKE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let file_layer = fmt::layer().with_writer(writer).with_ansi(false);
    let console_layer = console.then(|| fmt::layer().with_writer(std::io::stdout));
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(console_layer)
        .try_init()
        .context("installing tracing subscriber")?;
    Ok(guard)
}

/// Logs version, git hash, Windows build and GPU name.
pub fn log_startup_info() {
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        git = env!("RETAKE_GIT_HASH"),
        windows = %sysinfo::windows_build(),
        gpu = %sysinfo::gpu_name(),
        "Retake starting"
    );
}
