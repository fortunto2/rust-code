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

use std::cell::RefCell;
use std::future::Future;

/// Global JSONL log path — set during init, used by record_llm_span for file-based export.
static SPANS_LOG: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

// AI-NOTE: task-local only — no global fallback. Every task MUST run inside with_telemetry_scope().
tokio::task_local! {
    static TASK_SESSION_ID: RefCell<Option<String>>;
    static TASK_TASK_ID: RefCell<Option<String>>;
}

/// Set the current session ID (e.g. "t16_vm-abc123").
/// Panics if called outside `with_telemetry_scope`.
pub fn set_session_id(id: String) {
    TASK_SESSION_ID.with(|cell| {
        cell.replace(Some(id));
    });
}

/// Set the current task ID (e.g. "t16"). Attached to every LLM span.
/// Panics if called outside `with_telemetry_scope`.
pub fn set_task_id(id: String) {
    TASK_TASK_ID.with(|cell| {
        cell.replace(Some(id));
    });
}

/// Get current session ID (if set).
pub fn session_id() -> Option<String> {
    TASK_SESSION_ID
        .try_with(|cell| cell.borrow().clone())
        .ok()
        .flatten()
}

/// Get current task ID (if set).
fn task_id() -> Option<String> {
    TASK_TASK_ID
        .try_with(|cell| cell.borrow().clone())
        .ok()
        .flatten()
}

/// Wrap an async future with per-task telemetry scope.
/// Every tokio::spawn that uses set_session_id/set_task_id MUST be wrapped in this.
pub async fn with_telemetry_scope<F: Future>(fut: F) -> F::Output {
    TASK_SESSION_ID
        .scope(
            RefCell::new(None),
            TASK_TASK_ID.scope(RefCell::new(None), fut),
        )
        .await
}

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
    // Separate spans log — always written, independent of Phoenix
    let spans_path = format!("{}/{}-spans-{}.jsonl", log_dir, prefix, date);
    if let Ok(mut lock) = SPANS_LOG.write() {
        *lock = Some(spans_path);
    }

    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .unwrap_or_else(|e| panic!("Cannot open telemetry log {path}: {e}"));

    // Environment: OTEL_ENV or SOUFFLEUR_ENV, default "dev"
    let environment = std::env::var("OTEL_ENV")
        .or_else(|_| std::env::var("SOUFFLEUR_ENV"))
        .unwrap_or_else(|_| "dev".into());

    // Build tracer provider with resource identification
    // AI-NOTE: openinference.project.name required by Phoenix to route spans to named project
    let project_name = std::env::var("OTEL_PROJECT_NAME").unwrap_or_else(|_| prefix.to_string());
    let resource = opentelemetry_sdk::Resource::builder()
        .with_service_name(prefix.to_string())
        .with_attribute(opentelemetry::KeyValue::new(
            "deployment.environment",
            environment.clone(),
        ))
        .with_attribute(opentelemetry::KeyValue::new(
            "openinference.project.name",
            project_name,
        ))
        .build();
    let mut builder = SdkTracerProvider::builder().with_resource(resource);

    // Optional: OTLP batch exporter (LangSmith, Jaeger, Grafana, etc.)
    let otlp_enabled = if std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").is_ok() {
        // Parse OTEL_EXPORTER_OTLP_HEADERS (key=value,key=value)
        let mut headers = std::collections::HashMap::new();
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

        use opentelemetry_otlp::WithHttpConfig;
        match opentelemetry_otlp::SpanExporter::builder()
            .with_http()
            .with_headers(headers)
            .build()
        {
            Ok(exporter) => {
                // Use batch exporter — simple exporter calls reqwest::blocking inside async
                // context which creates a nested runtime and panics on drop.
                builder = builder.with_batch_exporter(exporter);
                let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").unwrap();
                let project = std::env::var("LANGSMITH_PROJECT").unwrap_or_default();
                eprintln!(
                    "[telemetry] OTLP exporter → {endpoint} [{environment}] project={project}"
                );
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

    // Register global provider so native OTEL spans (gen_ai.chat) get exported
    opentelemetry::global::set_tracer_provider(tracer_provider.clone());

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

/// Token usage from an LLM response.
pub struct LlmUsage {
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub cached_tokens: i64,
    pub response_model: String,
}

/// Record a single LLM span with OpenInference (Phoenix) + GenAI (LangSmith) conventions.
///
/// All three LLM clients (OxideClient, OxideChatClient, GenaiClient) call this after
/// receiving a response. One function, one set of attributes, one place to maintain.
pub fn record_llm_span(
    span_name: &str,
    model: &str,
    input: &str,
    output: &str,
    tool_calls: &[(String, String)],
    usage: &LlmUsage,
) {
    use opentelemetry::trace::{Span, Tracer, TracerProvider};

    let provider = opentelemetry::global::tracer_provider();
    let tracer = provider.tracer("sgr-agent");
    let mut span = tracer.start(span_name.to_string());

    let sid = session_id();
    if let Some(ref s) = sid {
        span.set_attribute(opentelemetry::KeyValue::new("session.id", s.clone()));
    }
    // task_id: explicit > parsed from session_id (part before first '_') > omit
    let tid = task_id().or_else(|| {
        sid.as_ref()
            .and_then(|s| s.split('_').next().map(String::from))
    });
    if let Some(t) = tid {
        span.set_attribute(opentelemetry::KeyValue::new("metadata.task_id", t));
    }

    // OpenInference conventions (Phoenix)
    span.set_attribute(opentelemetry::KeyValue::new(
        "openinference.span.kind",
        "LLM",
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "llm.model_name",
        model.to_string(),
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "llm.token_count.prompt",
        usage.prompt_tokens,
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "llm.token_count.completion",
        usage.completion_tokens,
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "llm.token_count.total",
        usage.prompt_tokens + usage.completion_tokens,
    ));
    if usage.cached_tokens > 0 {
        span.set_attribute(opentelemetry::KeyValue::new(
            "llm.token_count.cached",
            usage.cached_tokens,
        ));
    }

    // GenAI conventions (LangSmith)
    span.set_attribute(opentelemetry::KeyValue::new("langsmith.span.kind", "LLM"));
    span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.request.model",
        model.to_string(),
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.response.model",
        usage.response_model.clone(),
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.usage.prompt_tokens",
        usage.prompt_tokens,
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.usage.completion_tokens",
        usage.completion_tokens,
    ));

    // Input (last user/tool message, truncated)
    if !input.is_empty() {
        span.set_attribute(opentelemetry::KeyValue::new(
            "input.value",
            input.to_string(),
        ));
    }

    // Output: text content or tool calls as JSON
    let output_display = if !output.is_empty() {
        serde_json::json!({"role": "assistant", "content": output}).to_string()
    } else if !tool_calls.is_empty() {
        let calls: Vec<serde_json::Value> = tool_calls
            .iter()
            .map(|(name, args)| {
                let a = truncate_str(args, 200);
                serde_json::json!({"name": name, "arguments": a})
            })
            .collect();
        serde_json::json!({"role": "assistant", "tool_calls": calls}).to_string()
    } else {
        String::new()
    };
    if !output_display.is_empty() {
        span.set_attribute(opentelemetry::KeyValue::new("output.value", output_display));
        span.set_attribute(opentelemetry::KeyValue::new(
            "output.mime_type",
            "application/json",
        ));
    }

    span.end();

    // File-based export — always writes, even when Phoenix is down
    write_span_to_file(span_name, model, input, output, tool_calls, usage);
}

/// Append span as JSONL to `.agent/{prefix}-spans-{date}.jsonl`.
fn write_span_to_file(
    span_name: &str,
    model: &str,
    input: &str,
    output: &str,
    tool_calls: &[(String, String)],
    usage: &LlmUsage,
) {
    let path = match SPANS_LOG.read().ok().and_then(|l| l.clone()) {
        Some(p) => p,
        None => return,
    };
    let tc: Vec<&str> = tool_calls.iter().map(|(n, _)| n.as_str()).collect();
    let line = serde_json::json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "span": span_name,
        "model": model,
        "session_id": session_id().unwrap_or_default(),
        "task_id": task_id().unwrap_or_default(),
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "cached_tokens": usage.cached_tokens,
        "input": truncate_str(input, 200),
        "output": truncate_str(output, 200),
        "tool_calls": tc,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = writeln!(f, "{}", line);
    }
}

/// Record a trial result as an OTEL span with metadata attributes.
///
/// Creates a lightweight "trial.result" span with score, outcome, task_id, and steps
/// as span attributes. Phoenix shows these in the Attributes tab and they're searchable.
pub fn annotate_session(task_id: &str, score: f32, outcome: &str, steps: u32) {
    use opentelemetry::trace::{Span, Tracer, TracerProvider};

    let provider = opentelemetry::global::tracer_provider();
    let tracer = provider.tracer("sgr-agent");
    let mut span = tracer.start("trial.result".to_string());

    if let Some(sid) = session_id() {
        span.set_attribute(opentelemetry::KeyValue::new("session.id", sid));
    }
    span.set_attribute(opentelemetry::KeyValue::new(
        "openinference.span.kind",
        "EVALUATOR",
    ));
    span.set_attribute(opentelemetry::KeyValue::new("task_id", task_id.to_string()));
    span.set_attribute(opentelemetry::KeyValue::new("score", score as f64));
    span.set_attribute(opentelemetry::KeyValue::new("outcome", outcome.to_string()));
    span.set_attribute(opentelemetry::KeyValue::new("steps", steps as i64));
    span.set_attribute(opentelemetry::KeyValue::new(
        "input.value",
        format!("task: {task_id}"),
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "output.value",
        serde_json::json!({"score": score, "outcome": outcome, "steps": steps}).to_string(),
    ));
    span.set_attribute(opentelemetry::KeyValue::new(
        "output.mime_type",
        "application/json",
    ));
    span.end();

    // Post annotations to Phoenix REST API for all LLM spans in this session.
    // Simple exporter flushes spans synchronously on span.end(), so DB should have them already.
    if let (Some(endpoint), Some(sid)) = (
        std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT").ok(),
        session_id(),
    ) {
        post_session_annotations(&endpoint, &sid, task_id, score, outcome);
    }
}

/// Post annotations to Phoenix for all LLM spans in a session.
fn post_session_annotations(
    endpoint: &str,
    session_id: &str,
    task_id: &str,
    score: f32,
    outcome: &str,
) {
    let base = endpoint.trim_end_matches('/');
    let db_path = dirs::home_dir()
        .map(|h| h.join(".phoenix/phoenix.db"))
        .unwrap_or_default();
    let Ok(db) =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return;
    };

    // Find all LLM span_ids for this session
    let mut stmt = db
        .prepare(
            "SELECT s.span_id FROM spans s
         JOIN traces t ON s.trace_rowid = t.id
         JOIN project_sessions ps ON t.project_session_rowid = ps.id
         WHERE ps.session_id = ?1
           AND s.name IN ('chat.completions.api', 'oxide.responses.api')",
        )
        .unwrap();
    let span_ids: Vec<String> = stmt
        .query_map([session_id], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    if span_ids.is_empty() {
        return;
    }

    // Build annotations: task_id on all, score+outcome on last
    let mut data = Vec::new();
    for (i, sid) in span_ids.iter().enumerate() {
        data.push(serde_json::json!({
            "span_id": sid, "name": "task_id", "annotator_kind": "LLM",
            "result": {"explanation": task_id}
        }));
        if i == 0 {
            // first = most recent (DESC order)
            data.push(serde_json::json!({
                "span_id": sid, "name": "score", "annotator_kind": "LLM",
                "result": {"score": score}
            }));
            data.push(serde_json::json!({
                "span_id": sid, "name": "outcome", "annotator_kind": "LLM",
                "result": {"label": outcome}
            }));
        }
    }

    let client = reqwest::blocking::Client::new();
    // Span annotations (visible in Spans tab)
    let _ = client
        .post(format!("{base}/v1/span_annotations"))
        .json(&serde_json::json!({"data": data}))
        .send();

    // Trace annotations (visible in Traces tab) — annotate all traces in this session
    let trace_ids: Vec<String> = db
        .prepare(
            "SELECT DISTINCT t.trace_id FROM traces t
         JOIN project_sessions ps ON t.project_session_rowid = ps.id
         WHERE ps.session_id = ?1",
        )
        .unwrap()
        .query_map([session_id], |row| row.get(0))
        .unwrap()
        .filter_map(|r| r.ok())
        .collect();

    if !trace_ids.is_empty() {
        let mut trace_data = Vec::new();
        for tid in &trace_ids {
            trace_data.push(serde_json::json!({
                "trace_id": tid, "name": "task_id", "annotator_kind": "LLM",
                "result": {"explanation": task_id}
            }));
            trace_data.push(serde_json::json!({
                "trace_id": tid, "name": "score", "annotator_kind": "LLM",
                "result": {"score": score}
            }));
            trace_data.push(serde_json::json!({
                "trace_id": tid, "name": "outcome", "annotator_kind": "LLM",
                "result": {"label": outcome}
            }));
        }
        let _ = client
            .post(format!("{base}/v1/trace_annotations"))
            .json(&serde_json::json!({"data": trace_data}))
            .send();
    }
}

/// Truncate a string to `max_len` bytes, appending "..." if truncated.
fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() > max_len {
        format!("{}...", &s[..max_len])
    } else {
        s.to_string()
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
