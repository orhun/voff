//! Content-free application diagnostics, written off the render thread.
use anyhow::{Context, Result};
use std::{fs::OpenOptions, path::Path};
use tracing_subscriber::{filter::filter_fn, prelude::*};

pub fn init(workspace: &Path) -> Result<tracing_appender::non_blocking::WorkerGuard> {
    let directory = workspace.join(".voff/logs");
    std::fs::create_dir_all(&directory).context("could not create diagnostic log directory")?;
    let path = directory.join("voff.log");
    // Keep one previous log and bound growth between launches.
    if std::fs::metadata(&path).is_ok_and(|m| m.len() >= 10 * 1024 * 1024) {
        std::fs::rename(&path, directory.join("voff.previous.log"))?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .context("could not open diagnostic log")?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let level = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(level)
        // SDK debug traces may contain prompts or HTTP bodies. Only our
        // deliberately metadata-only instrumentation belongs in this log.
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter_fn(|metadata| metadata.target().starts_with("voff"))),
        )
        .try_init()
        .context("could not initialize diagnostic logging")?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "application started");
    Ok(guard)
}
