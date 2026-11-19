//! Logging setup (specs/01-architecture.md). Never log tokens, secrets or raw keystrokes.

use std::io;
use std::path::Path;

use tracing::level_filters::LevelFilter;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::prelude::*;

/// Installs the global subscriber: stderr at a level chosen by `-v` count, plus a daily
/// `aftercast.YYYY-MM-DD.log` in `logs_dir` (at least INFO) when that folder exists.
pub fn init(logs_dir: &Path, verbosity: u8) {
    let stderr_level = match verbosity {
        0 => LevelFilter::WARN,
        1 => LevelFilter::INFO,
        _ => LevelFilter::DEBUG,
    };
    // Checked first: the appender would create a missing folder, i.e. a home before `init`.
    let file = logs_dir
        .is_dir()
        .then(|| {
            RollingFileAppender::builder()
                .rotation(Rotation::DAILY)
                .filename_prefix("aftercast")
                .filename_suffix("log")
                .build(logs_dir)
                .ok()
        })
        .flatten()
        .map(|appender| {
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(appender)
                .with_filter(stderr_level.max(LevelFilter::INFO))
        });
    let stderr = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_target(false)
        .with_filter(stderr_level);
    tracing_subscriber::registry()
        .with(stderr)
        .with(file)
        .init();
}
