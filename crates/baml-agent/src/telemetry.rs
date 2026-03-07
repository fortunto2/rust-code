//! OpenTelemetry file telemetry for BAML agents.
//!
//! Single JSONL file per day with OTEL trace context (trace_id, span_id)
//! in every line. Use `tracing::info_span!()` in agent code to create
//! correlated traces across LLM calls, tool executions, and coaching turns.
//!
//! To switch from file to Jaeger/Grafana later: replace the JSON fmt layer
//! with an OTLP exporter — no instrumentation changes needed.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::prelude::*;

/// Initialize OTEL-aware file telemetry.
///
/// Output: `{log_dir}/{prefix}-YYYY-MM-DD.jsonl`
///
/// Each JSON line includes: timestamp, level, target, message, fields,
/// span context (name, trace_id, span_id), and any custom attributes.
///
/// ```ignore
/// let _guard = baml_agent::init_telemetry(".agent", "coach");
///
/// let span = tracing::info_span!("coaching_turn", turn = 3);
/// let _enter = span.enter();
/// tracing::info!(model = "gemini-flash", latency_ms = 420, "LLM response");
/// // → {"timestamp":"...", "level":"INFO", "spans":[{"name":"coaching_turn","turn":3}],
/// //    "fields":{"model":"gemini-flash","latency_ms":420}, "message":"LLM response"}
/// ```
pub fn init_telemetry(log_dir: &str, prefix: &str) -> TelemetryGuard {
    let _ = std::fs::create_dir_all(log_dir);

    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let path = format!("{}/{}-{}.jsonl", log_dir, prefix, date);

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("Cannot open telemetry log {path}: {e}"));

    // OTEL tracer for span context propagation (trace_id / span_id)
    let tracer_provider = SdkTracerProvider::builder().build();
    let tracer = tracer_provider.tracer(prefix.to_string());

    // Layer 1: OTEL context → attaches trace_id/span_id to tracing spans
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    // Layer 2: JSON file output
    let json_layer = tracing_subscriber::fmt::layer()
        .json()
        .with_writer(std::sync::Mutex::new(file))
        .with_target(true)
        .with_thread_ids(false)
        .with_span_list(true);

    // Filter: info+ by default, suppress noisy HTTP internals
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new("info")
            .add_directive("hyper=off".parse().unwrap())
            .add_directive("h2=off".parse().unwrap())
            .add_directive("reqwest=off".parse().unwrap())
    });

    tracing_subscriber::registry()
        .with(filter)
        .with(otel_layer)
        .with(json_layer)
        .init();

    // Bridge log crate → tracing (captures library log::info!/warn!/etc)
    let _ = tracing_log::LogTracer::init();

    // Suppress BAML's direct stderr output (it bypasses log/tracing)
    super::suppress_baml_log();

    TelemetryGuard { tracer_provider }
}

/// Must be held alive for the duration of the program.
/// Flushes pending spans on drop.
pub struct TelemetryGuard {
    tracer_provider: SdkTracerProvider,
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        let _ = self.tracer_provider.shutdown();
    }
}
