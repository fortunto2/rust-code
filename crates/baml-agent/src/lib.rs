pub mod config;
pub mod engine;
pub mod session;
pub mod loop_detect;
pub mod agent_loop;
pub mod prompt;
pub mod helpers;
#[cfg(feature = "logging")]
pub mod logging;

pub use config::{AgentConfig, ProviderConfig, AgentConfigError};
pub use engine::{BamlRegistry, AgentEngine};
pub use session::{AgentMessage, MessageRole, Session, SessionMeta, list_sessions};
#[cfg(feature = "search")]
pub use session::search_sessions;
pub use loop_detect::{LoopDetector, LoopStatus, normalize_signature};
pub use agent_loop::{SgrAgent, SgrAgentStream, StepDecision, ActionResult, LoopConfig, LoopEvent, run_loop, run_loop_stream, process_step};
pub use prompt::{BASE_SYSTEM_PROMPT, build_system_prompt};
pub use helpers::{norm, norm_owned, action_result_json, action_result_from, action_result_done, truncate_json_array, load_manifesto, load_manifesto_from};
#[cfg(feature = "logging")]
pub use logging::init_logging;

/// Suppress BAML's default stdout logging (prompts, responses, timing).
/// Call once at startup before any BAML calls.
pub fn suppress_baml_log() {
    // SAFETY: single-threaded init, before any BAML calls
    unsafe { std::env::set_var("BAML_LOG", "off"); }
}
