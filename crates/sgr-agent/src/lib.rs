//! # sgr-agent — LLM client + agent framework
//!
//! Pure Rust. No dlopen, no external binaries.
//! Works on iOS, Android, WASM — anywhere reqwest+rustls compiles.
//!
//! ## LLM Client (default)
//! - **Structured output** — response conforms to JSON Schema (SGR envelope)
//! - **Function calling** — tools as typed structs, model picks & fills params
//! - **Flexible parser** — extract JSON from markdown, broken JSON, streaming chunks
//! - **Backends**: Gemini (Google AI + Vertex AI), OpenAI (+ OpenRouter, Ollama)
//!
//! ## Agent Framework (`feature = "agent"`)
//! - **Tool trait** — define tools with typed args + async execute
//! - **ToolRegistry** — ordered collection, case-insensitive lookup, fuzzy resolve
//! - **Agent trait** — decides what tools to call given conversation history
//! - **3 agent variants**: SgrAgent (structured output), ToolCallingAgent (native FC), FlexibleAgent (text parse)
//! - **Agent loop** — decide → execute → feed back, with 3-tier loop detection
//! - **Progressive discovery** — filter tools by relevance (TF-IDF scoring)

pub mod baml_parser;
pub mod codegen;
pub mod coerce;
pub mod flexible_parser;
pub mod schema;
pub mod tool;
pub mod types;

#[cfg(feature = "gemini")]
pub mod gemini;

#[cfg(feature = "openai")]
pub mod openai;

#[cfg(feature = "genai")]
pub(crate) mod genai_client;
#[cfg(feature = "genai")]
pub mod llm;
#[cfg(feature = "genai")]
pub use llm::Llm;

// Agent framework (behind feature gate)
#[cfg(feature = "agent")]
pub mod agent;
#[cfg(feature = "agent")]
pub mod agent_loop;
#[cfg(feature = "agent")]
pub mod agent_tool;
#[cfg(feature = "agent")]
pub mod agents;
#[cfg(feature = "agent")]
pub mod client;
#[cfg(feature = "agent")]
pub mod compaction;
#[cfg(feature = "agent")]
pub mod context;
#[cfg(feature = "agent")]
pub mod discovery;
#[cfg(feature = "agent")]
pub mod factory;
#[cfg(feature = "agent")]
pub mod prompt_loader;
#[cfg(feature = "agent")]
pub mod registry;
#[cfg(feature = "agent")]
pub mod retry;
#[cfg(feature = "agent")]
pub mod router;
#[cfg(feature = "agent")]
pub mod schema_simplifier;
#[cfg(feature = "agent")]
pub mod streaming;
#[cfg(feature = "agent")]
pub mod swarm;
#[cfg(feature = "agent")]
pub mod swarm_tools;
#[cfg(feature = "agent")]
pub mod union_schema;

// Session / app modules (from baml-agent migration)
#[cfg(feature = "session")]
pub mod app_config;
#[cfg(feature = "session")]
pub mod app_loop;
#[cfg(feature = "session")]
pub mod doctor;
#[cfg(feature = "session")]
pub mod hints;
#[cfg(feature = "session")]
pub mod intent_guard;
#[cfg(feature = "session")]
pub mod loop_detect;
#[cfg(feature = "session")]
pub mod memory;
#[cfg(feature = "session")]
pub mod prompt_template;
#[cfg(feature = "session")]
pub mod session;
#[cfg(feature = "session")]
pub mod tasks;

#[cfg(feature = "app-tools")]
pub mod app_tools;

pub mod benchmark;
pub mod evolution;
pub mod openapi;

#[cfg(feature = "providers")]
pub mod providers;

#[cfg(feature = "telemetry")]
pub mod telemetry;

#[cfg(feature = "logging")]
pub mod logging;

// Re-exports from session modules
#[cfg(feature = "session")]
pub use app_config::{AgentConfig, AgentConfigError};
#[cfg(feature = "session")]
pub use app_loop::{
    ActionResult, LoopConfig, LoopEvent, SgrAgent, SgrAgentStream, StepDecision, process_step,
    run_loop, run_loop_stream,
};
#[cfg(feature = "session")]
pub use doctor::{
    CheckResult, CheckStatus, DoctorCheck, check_gcloud_adc, check_provider_auth,
    default_tool_checks, fix_missing, format_check, optional_tool_checks, print_doctor_report,
    run_doctor, run_tool_check,
};
#[cfg(feature = "session")]
pub use hints::{
    HintContext, HintSource, PatternHints, TaskHints, ToolHints, WorkflowHints, collect_hints,
    default_sources, default_sources_with_tasks,
};
#[cfg(feature = "session")]
pub use intent_guard::{ActionKind, Intent, IntentCheck, guard_step, intent_allows};
#[cfg(feature = "logging")]
pub use logging::init_logging;
#[cfg(feature = "session")]
pub use loop_detect::{LoopDetector, LoopStatus, normalize_signature};
#[cfg(feature = "session")]
pub use memory::{
    MemoryContext, action_result_done, action_result_from, action_result_json, load_context_dir,
    load_manifesto, load_manifesto_from, norm, norm_owned, truncate_json_array,
};
#[cfg(feature = "session")]
pub use prompt_template::{BASE_SYSTEM_PROMPT, build_system_prompt};
#[cfg(all(feature = "search", feature = "session"))]
pub use session::search_sessions;
#[cfg(feature = "session")]
pub use session::{
    AgentMessage, EntryType, MessageRole, Session, SessionHeader, SessionMeta,
    import_claude_session, list_sessions,
};
#[cfg(feature = "session")]
pub use tasks::{
    Priority, Task, TaskStatus, append_notes, create_task, load_tasks, save_task, tasks_context,
    tasks_dir, tasks_summary, update_status,
};
#[cfg(feature = "telemetry")]
pub use telemetry::{TelemetryGuard, init_telemetry};

pub use coerce::coerce_value;
pub use flexible_parser::{parse_flexible, parse_flexible_coerced};
pub use schema::{json_schema_for, response_schema_for};
pub use tool::{ToolDef, tool};
pub use types::*;
