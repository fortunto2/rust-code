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
    AgentEventHandler, AgentTaskEvent, ChannelHandler, TuiAgent, ViewableContent,
    extract_viewable_json, spawn_agent_loop,
};
pub use chat::ChatState;
pub use command_palette::CommandPalette;
pub use content_viewer::ContentViewer;
pub use context_bar::ProjectContext;
pub use event::AppEvent;
pub use focus::{FocusLayer, FocusResult, FocusRing, point_in_rect, route_key, route_mouse};
pub use headless::run_headless;
pub use help::HelpOverlay;
pub use picker::{FuzzyPicker, PickerAction, PickerItem, PickerPreview};
pub use terminal::{Tui, init_terminal, restore_terminal, setup_panic_hook};
#[cfg(unix)]
pub use terminal::{TuiTelemetryGuard, init_tui_telemetry};

// Re-export sgr-agent essentials for convenience
pub use sgr_agent::{
    ActionResult, AgentConfig, AgentMessage, LoopConfig, LoopEvent, MessageRole, Session, SgrAgent,
    SgrAgentStream, StepDecision, run_loop, run_loop_stream,
};
