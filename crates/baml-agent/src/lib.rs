pub mod config;
pub mod engine;
pub mod session;
pub mod loop_detect;
pub mod agent_loop;
pub mod prompt;

pub use config::{AgentConfig, ProviderConfig, AgentConfigError};
pub use engine::{BamlRegistry, AgentEngine};
pub use session::{AgentMessage, MessageRole, Session};
pub use loop_detect::{LoopDetector, LoopStatus};
pub use agent_loop::{SgrAgent, SgrAgentStream, StepDecision, ActionResult, LoopConfig, LoopEvent, run_loop, run_loop_stream};
pub use prompt::{BASE_SYSTEM_PROMPT, build_system_prompt};
