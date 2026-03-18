//! OpenTelemetry file telemetry + optional OTLP export for LLM agents.
//!
//! Always: JSONL file per day with OTEL trace context (trace_id, span_id).
//! Optional: OTLP/HTTP batch exporter when `OTEL_EXPORTER_OTLP_ENDPOINT` is set.
//!
//! For LangSmith:
//! ```bash
//! OTEL_EXPORTER_OTLP_ENDPOINT=https://api.smith.langchain.com/otel
//! OTEL_EXPORTER_OTLP_HEADERS=x-api-key=lsv2_pt_...
//! ```
//!
//! **IMPORTANT**: Call `TelemetryGuard::shutdown()` (or `drop(guard)`) before
//! returning from `#[tokio::main]`. The OTLP batch exporter needs the tokio
//! runtime alive to flush pending spans over HTTP.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::prelude::*;

/// Initialize OTEL-aware file telemetry + optional OTLP export.
///
/// Output: `{log_dir}/{prefix}-YYYY-MM-DD.jsonl`
///
/// When `OTEL_EXPORTER_OTLP_ENDPOINT` env var is set, also exports spans
/// via OTLP/HTTP (protobuf) to the configured endpoint. Headers from
/// `OTEL_EXPORTER_OTLP_HEADERS` are included automatically (standard OTEL SDK).
///
/// ```ignore
/// let guard = sgr_agent::init_telemetry(".agent", "coach");
/// // ... do work ...
/// guard.shutdown(); // flush OTLP spans before tokio exits
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

    // Build tracer provider with resource identification
    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(prefix.to_string())
        .build();
    let mut builder = SdkTracerProvider::builder().with_resource(resource);

    // Optional: OTLP batch exporter (LangSmith, Jaeger, Grafana, etc.)
    let otlp_enabled = if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        use opentelemetry_otlp::WithHttpConfig;
        use std::collections::HashMap;

        // Build custom headers: auth + LangSmith project routing
        let mut headers = HashMap::new();

        // Parse OTEL_EXPORTER_OTLP_HEADERS (key=value,key=value)
        if let Ok(raw) = std::env::var("OTEL_EXPORTER_OTLP_HEADERS") {
            for pair in raw.split(',') {
                if let Some((k, v)) = pair.split_once('=') {
                    headers.insert(k.trim().to_string(), v.trim().to_string());
                }
            }
        }

        // LANGSMITH_PROJECT env var → Langsmith-Project header
        if let Ok(project) = std::env::var("LANGSMITH_PROJECT") {
            headers.insert("Langsmith-Project".to_string(), project);
        }

        match opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_headers(headers)
            .build()
        {
            Ok(exporter) => {
                builder = builder.with_batch_exporter(exporter);
                let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap();
                eprintln!("[telemetry] OTLP exporter → {endpoint}");
                true
            }
            Err(e) => {
                eprintln!("[telemetry] OTLP exporter failed: {e}");
                false
            }
        }
    } else {
        false
    };

    let tracer_provider = builder.build();
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

    // Layer 3: compact stderr output for Xcode console / `log stream --device`
    let stderr_layer = tracing_subscriber::fmt::layer()
        .compact()
        .with_target(false)
        .with_writer(std::io::stderr);

    tracing_subscriber::registry()
        .with(filter)
        .with(otel_layer)
        .with(json_layer)
        .with(stderr_layer)
        .init();

    // Bridge log crate → tracing (captures library log::info!/warn!/etc)
    let _ = tracing_log::LogTracer::init();

    TelemetryGuard {
        tracer_provider,
        otlp_enabled,
    }
}

/// Must be held alive for the duration of the program.
/// Flushes pending spans on drop.
///
/// **IMPORTANT**: For OTLP batch export, call `shutdown()` explicitly before
/// the tokio runtime exits. The batch exporter needs tokio to flush HTTP requests.
/// Dropping inside `#[tokio::main]` async fn (before `Ok(())`) is correct.
/// Dropping after tokio shuts down will silently lose spans.
pub struct TelemetryGuard {
    tracer_provider: SdkTracerProvider,
    otlp_enabled: bool,
}

impl TelemetryGuard {
    /// Whether OTLP export is active (endpoint was configured and exporter initialized).
    pub fn otlp_enabled(&self) -> bool {
        self.otlp_enabled
    }

    /// Explicitly flush and shutdown. Consumes self.
    ///
    /// Call this before returning from `#[tokio::main]` to ensure the batch
    /// exporter flushes all pending spans while the tokio runtime is still alive.
    pub fn shutdown(self) {
        // Drop triggers tracer_provider.shutdown()
        drop(self);
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Err(e) = self.tracer_provider.shutdown() {
            eprintln!("[telemetry] shutdown error: {e}");
        }
    }
}
