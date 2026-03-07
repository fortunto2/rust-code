pub mod agent_task;
pub mod chat;
pub mod command_palette;
pub mod content_viewer;
pub mod context_bar;
pub mod event;
pub mod focus;
pub mod headless;
pub mod help;
pub mod picker;
pub mod terminal;

pub use agent_task::{
    spawn_agent_loop, AgentEventHandler, AgentTaskEvent, ChannelHandler, TuiAgent,
};
pub use chat::ChatState;
pub use command_palette::CommandPalette;
pub use content_viewer::ContentViewer;
pub use context_bar::ProjectContext;
pub use event::AppEvent;
pub use focus::{point_in_rect, route_key, route_mouse, FocusLayer, FocusResult};
pub use headless::run_headless;
pub use help::HelpOverlay;
pub use picker::{FuzzyPicker, PickerAction, PickerItem, PickerPreview};
pub use terminal::{init_terminal, restore_terminal, setup_panic_hook, Tui};
#[cfg(unix)]
pub use terminal::{init_tui_telemetry, TuiTelemetryGuard};

// Re-export baml-agent essentials for convenience
pub use baml_agent::{
    run_loop, run_loop_stream, ActionResult, AgentConfig, AgentEngine, AgentMessage, BamlRegistry,
    LoopConfig, LoopEvent, MessageRole, Session, SgrAgent, SgrAgentStream, StepDecision,
};
