pub mod agent_task;
pub mod chat;
pub mod event;
pub mod headless;
pub mod terminal;

pub use agent_task::{
    spawn_agent_loop, AgentEventHandler, AgentTaskEvent, ChannelHandler, TuiAgent,
};
pub use chat::ChatState;
pub use event::AppEvent;
pub use headless::run_headless;
pub use terminal::{init_terminal, restore_terminal, setup_panic_hook, Tui};

// Re-export baml-agent essentials for convenience
pub use baml_agent::{
    run_loop, run_loop_stream, ActionResult, AgentConfig, AgentEngine, AgentMessage, BamlRegistry,
    LoopConfig, LoopEvent, MessageRole, Session, SgrAgent, SgrAgentStream, StepDecision,
};
