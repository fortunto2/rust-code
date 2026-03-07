pub mod config;
pub mod engine;
pub mod session;
pub mod loop_detect;
pub mod agent_loop;
pub mod prompt;
#[cfg(feature = "logging")]
pub mod logging;

pub use config::{AgentConfig, ProviderConfig, AgentConfigError};
pub use engine::{BamlRegistry, AgentEngine};
pub use session::{AgentMessage, MessageRole, Session};
pub use loop_detect::{LoopDetector, LoopStatus};
pub use agent_loop::{SgrAgent, SgrAgentStream, StepDecision, ActionResult, LoopConfig, LoopEvent, run_loop, run_loop_stream, process_step};
pub use prompt::{BASE_SYSTEM_PROMPT, build_system_prompt};
#[cfg(feature = "logging")]
pub use logging::init_logging;

/// Suppress BAML's default stdout logging (prompts, responses, timing).
/// Call once at startup before any BAML calls.
pub fn suppress_baml_log() {
    // SAFETY: single-threaded init, before any BAML calls
    unsafe { std::env::set_var("BAML_LOG", "off"); }
}
