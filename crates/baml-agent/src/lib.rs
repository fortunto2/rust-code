pub mod agent_loop;
pub mod config;
pub mod engine;
pub mod helpers;
#[cfg(feature = "logging")]
pub mod logging;
pub mod loop_detect;
pub mod prompt;
pub mod session;
#[cfg(feature = "telemetry")]
pub mod telemetry;

pub use agent_loop::{
    process_step, run_loop, run_loop_stream, ActionResult, LoopConfig, LoopEvent, SgrAgent,
    SgrAgentStream, StepDecision,
};
pub use config::{AgentConfig, AgentConfigError, ProviderConfig};
pub use engine::{AgentEngine, BamlRegistry};
pub use helpers::{
    action_result_done, action_result_from, action_result_json, load_context_dir, load_manifesto,
    load_manifesto_from, norm, norm_owned, truncate_json_array, AgentContext,
};
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

/// Suppress BAML's default stdout logging (prompts, responses, timing).
/// Call once at startup before any BAML calls.
pub fn suppress_baml_log() {
    // SAFETY: single-threaded init, before any BAML calls
    unsafe {
        std::env::set_var("BAML_LOG", "off");
    }
}
