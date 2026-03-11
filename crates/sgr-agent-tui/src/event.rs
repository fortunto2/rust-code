use crossterm::event::Event;

/// Core TUI events shared across all agent apps.
///
/// Projects can extend this with their own events using the `Custom(T)` variant.
pub enum AppEvent<T = ()> {
    /// Terminal UI event (key press, mouse, resize).
    Ui(Event),
    /// Periodic tick for animations/spinners.
    Tick,
    /// Streaming text chunk from LLM (append to current message).
    StreamChunk(String),
    /// Complete agent response message.
    AgentMessage(String),
    /// Agent plan updates.
    AgentPlan(Vec<String>),
    /// Agent finished processing.
    AgentDone,
    /// A file was modified by a tool (for preview refresh, git status, etc.).
    FileModified(String),
    /// Project-specific custom event.
    Custom(T),
}
