pub mod terminal;
pub mod event;
pub mod chat;
pub mod agent_task;
pub mod headless;

pub use terminal::{Tui, init_terminal, restore_terminal, setup_panic_hook};
pub use event::AppEvent;
pub use chat::ChatState;
pub use agent_task::{
    AgentTaskEvent, AgentEventHandler, ChannelHandler,
    TuiAgent, spawn_agent_loop,
};
pub use headless::run_headless;

// Re-export baml-agent essentials for convenience
pub use baml_agent::{
    SgrAgent, SgrAgentStream, StepDecision, ActionResult,
    LoopConfig, LoopEvent, run_loop, run_loop_stream,
    Session, AgentMessage, MessageRole,
    AgentConfig, AgentEngine, BamlRegistry,
};
