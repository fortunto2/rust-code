pub mod agent_loop;
pub mod config;
pub mod doctor;
pub mod engine;
pub mod helpers;
pub mod hints;
pub mod intent_guard;
#[cfg(feature = "logging")]
pub mod logging;
pub mod loop_detect;
pub mod prompt;
#[cfg(feature = "providers")]
pub mod providers;
pub mod session;
#[cfg(feature = "telemetry")]
pub mod telemetry;

pub use agent_loop::{
    process_step, run_loop, run_loop_stream, ActionResult, LoopConfig, LoopEvent, SgrAgent,
    SgrAgentStream, StepDecision,
};
pub use config::{AgentConfig, AgentConfigError, ProviderConfig};
pub use doctor::{
    check_gcloud_adc, check_provider_auth, default_tool_checks, fix_missing, format_check,
    optional_tool_checks, print_doctor_report, run_doctor, run_tool_check, CheckResult,
    CheckStatus, DoctorCheck,
};
pub use engine::{AgentEngine, BamlRegistry};
pub use helpers::{
    action_result_done, action_result_from, action_result_json, load_context_dir, load_manifesto,
    load_manifesto_from, norm, norm_owned, truncate_json_array, AgentContext,
};
pub use hints::{
    collect_hints, default_sources, HintContext, HintSource, PatternHints, ToolHints, WorkflowHints,
};
pub use intent_guard::{guard_step, intent_allows, ActionKind, Intent, IntentCheck};
#[cfg(feature = "logging")]
pub use logging::init_logging;
pub use loop_detect::{normalize_signature, LoopDetector, LoopStatus};
pub use prompt::{build_system_prompt, BASE_SYSTEM_PROMPT};
#[cfg(feature = "search")]
pub use session::search_sessions;
pub use session::{
    import_claude_session, list_sessions, AgentMessage, EntryType, MessageRole, Session,
    SessionMeta,
};
#[cfg(feature = "telemetry")]
pub use telemetry::{init_telemetry, TelemetryGuard};

/// Suppress BAML's default stderr logging (prompts, responses, timing).
///
/// Respects existing `BAML_LOG` env var — if already set, does nothing.
/// For debug mode: `BAML_LOG=debug cargo run` shows full prompts/responses on stderr.
pub fn suppress_baml_log() {
    if std::env::var("BAML_LOG").is_ok() {
        return; // user explicitly set BAML_LOG, respect it
    }
    // SAFETY: single-threaded init, before any BAML calls
    unsafe {
        std::env::set_var("BAML_LOG", "off");
    }
}
