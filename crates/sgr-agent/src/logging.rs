use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;

/// Initialize file-based JSONL logging + suppress BAML stdout noise.
///
/// Logs go to `{log_dir}/{prefix}-YYYY-MM-DD.jsonl`.
/// Returns a guard that must be held until shutdown (flushes on drop).
///
/// # Usage
/// ```ignore
/// let _guard = sgr_agent::init_logging(".my-agent", "agent");
/// // all tracing::info!(), warn!(), error!() go to file
/// // BAML stdout logging suppressed
/// ```
pub fn init_logging(log_dir: &str, prefix: &str) -> WorkerGuard {
    let _ = std::fs::create_dir_all(log_dir);

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let filename = format!("{}-{}.jsonl", prefix, date);

    let file_appender = tracing_appender::rolling::never(log_dir, filename);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(non_blocking)
                .with_target(false)
                .with_thread_ids(false),
        )
        .init();

    guard
}
