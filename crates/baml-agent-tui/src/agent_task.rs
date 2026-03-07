use baml_agent::{
    process_step, AgentMessage, LoopConfig, LoopDetector, LoopEvent, MessageRole, Session,
    SgrAgentStream,
};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Viewable content extracted from tool output — for Ctrl+O viewer.
#[derive(Debug, Clone)]
pub struct ViewableContent {
    /// Short title (e.g. filename).
    pub title: String,
    /// Chat preview (1-3 lines).
    pub preview: String,
    /// Full content for the viewer overlay.
    pub full_content: String,
}

/// Events emitted by the agent task back to the TUI.
pub enum AgentTaskEvent {
    /// Step started (1-based).
    StepStart(usize),
    /// Streaming text chunk from LLM.
    StreamChunk(String),
    /// LLM decision: situation + task (STAR).
    Decision {
        situation: String,
        task: Vec<String>,
    },
    /// About to execute an action (human-readable label).
    ActionStart(String),
    /// Action executed, short result (truncated for chat).
    ActionDone(String),
    /// Action produced viewable content (file contents, etc.) — preview in chat, full via Ctrl+O.
    ActionViewable(ViewableContent),
    /// A file was modified by a tool action.
    FileModified(String),
    /// Context trimmed.
    Trimmed(usize),
    /// Warning (loop detected, etc).
    Warning(String),
    /// Error message.
    Error(String),
    /// Task completed by LLM.
    Completed,
    /// Agent loop finished (always sent last).
    Done,
}

/// Callback trait for handling agent events in the TUI.
///
/// Implement this to map agent events to your TUI's event system.
/// Methods are sync — they typically just send to a channel.
pub trait AgentEventHandler: Send + Sync + 'static {
    /// Called for each agent task event. Return false to stop the loop.
    fn on_event(&self, event: AgentTaskEvent) -> bool;
}

/// Channel-based event handler — maps AgentTaskEvent to your AppEvent type.
pub struct ChannelHandler<T: Send + 'static> {
    tx: tokio::sync::mpsc::Sender<T>,
    mapper: Box<dyn Fn(AgentTaskEvent) -> T + Send + Sync>,
}

impl<T: Send + 'static> ChannelHandler<T> {
    pub fn new(
        tx: tokio::sync::mpsc::Sender<T>,
        mapper: impl Fn(AgentTaskEvent) -> T + Send + Sync + 'static,
    ) -> Self {
        Self {
            tx,
            mapper: Box::new(mapper),
        }
    }
}

impl<T: Send + 'static> AgentEventHandler for ChannelHandler<T> {
    fn on_event(&self, event: AgentTaskEvent) -> bool {
        let mapped = (self.mapper)(event);
        self.tx.try_send(mapped).is_ok()
    }
}

/// TUI-specific agent extension.
///
/// Extends `SgrAgentStream` with optional methods for TUI display.
/// Default implementations work out of the box — override for richer UI.
pub trait TuiAgent: SgrAgentStream {
    /// Human-readable action label for display. Defaults to `action_signature`.
    fn action_label(action: &Self::Action) -> String {
        Self::action_signature(action)
    }

    /// If an action modifies a file, return the path (for TUI refresh / git status).
    fn file_modified(_action: &Self::Action) -> Option<String> {
        None
    }

    /// Extract viewable content from tool output for the Ctrl+O viewer.
    ///
    /// Override to detect project-specific JSON patterns (read_file, etc.).
    /// Default: tries common JSON patterns (`{"operation":"read_file","content":"..."}`).
    fn viewable_content(output: &str) -> Option<ViewableContent> {
        extract_viewable_json(output)
    }
}

/// Default viewable content extractor for common JSON tool output patterns.
///
/// Recognized patterns:
/// - `{"operation": "read_file", "path": "...", "content": "..."}`
pub fn extract_viewable_json(output: &str) -> Option<ViewableContent> {
    let json: serde_json::Value = serde_json::from_str(output).ok()?;
    let op = json.get("operation")?.as_str()?;
    match op {
        "read_file" => {
            let content = json.get("content")?.as_str()?;
            let path = json.get("path").and_then(|v| v.as_str()).unwrap_or("file");
            let filename = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string());
            let line_count = content.lines().count();
            let preview_lines: String = content.lines().take(3).collect::<Vec<_>>().join(" | ");
            let preview = if line_count > 3 {
                format!(
                    "  = {}... [{} lines \u{2014} Ctrl+O view]",
                    preview_lines, line_count
                )
            } else {
                format!("  = {}", content)
            };
            Some(ViewableContent {
                title: filename,
                preview,
                full_content: content.to_string(),
            })
        }
        _ => None,
    }
}

/// Run the agent loop as a tokio task with TUI event integration.
///
/// Key design: session is **unlocked during LLM streaming** (so TUI can
/// read messages for display) and **locked during action execution**
/// (which modifies session).
///
/// # Arguments
/// - `agent`: Shared agent ref. Takes `&self`, so `Arc` suffices (no Mutex).
///   Use interior mutability (Mutex) inside agent for stateful tools (MCP, etc).
/// - `session`: Shared session behind Mutex. TUI can lock to read messages.
/// - `pending_notes`: Queue of user messages injected between steps.
/// - `handler`: Receives `AgentTaskEvent`s — typically a `ChannelHandler`.
/// - `config`: Loop config (max_steps, loop_abort_threshold).
pub fn spawn_agent_loop<A, H>(
    agent: Arc<A>,
    session: Arc<Mutex<Session<A::Msg>>>,
    pending_notes: Arc<Mutex<Vec<String>>>,
    handler: H,
    config: LoopConfig,
) -> tokio::task::JoinHandle<()>
where
    A: TuiAgent + Send + Sync + 'static,
    H: AgentEventHandler,
{
    tokio::spawn(async move {
        let result = run_tui_loop(&*agent, &session, &pending_notes, &handler, &config).await;

        if let Err(e) = result {
            handler.on_event(AgentTaskEvent::Error(format!("Agent error: {}", e)));
        }
        handler.on_event(AgentTaskEvent::Done);
    })
}

/// Inner loop logic. Separated for testability.
async fn run_tui_loop<A, H>(
    agent: &A,
    session: &Mutex<Session<A::Msg>>,
    pending_notes: &Mutex<Vec<String>>,
    handler: &H,
    config: &LoopConfig,
) -> Result<usize, String>
where
    A: TuiAgent + Send + Sync,
    H: AgentEventHandler,
{
    let mut detector = LoopDetector::new(config.loop_abort_threshold);

    for step_num in 1..=config.max_steps {
        // --- Inject pending user notes ---
        {
            let notes: Vec<String> = std::mem::take(&mut *pending_notes.lock().await);
            if !notes.is_empty() {
                let mut sess = session.lock().await;
                for note in &notes {
                    sess.push(
                        <<A::Msg as AgentMessage>::Role>::user(),
                        format!("User note while task is running:\n{}", note),
                    );
                }
                handler.on_event(AgentTaskEvent::Warning(format!(
                    "[NOTE] {} queued note(s) injected",
                    notes.len()
                )));
            }
        }

        // --- Trim context ---
        {
            let mut sess = session.lock().await;
            let trimmed = sess.trim();
            if trimmed > 0 {
                handler.on_event(AgentTaskEvent::Trimmed(trimmed));
            }
        }

        if !handler.on_event(AgentTaskEvent::StepStart(step_num)) {
            return Ok(step_num); // TUI requested stop
        }

        // --- Snapshot messages for LLM (session UNLOCKED during streaming) ---
        let messages: Vec<A::Msg> = {
            let sess = session.lock().await;
            sess.messages().to_vec()
        };

        // --- Stream LLM decision ---
        let decision = agent
            .decide_stream(&messages, |token| {
                handler.on_event(AgentTaskEvent::StreamChunk(token.to_string()));
            })
            .await
            .map_err(|e| format!("{}", e))?;

        // --- Process step (session LOCKED during execution) ---
        let mut sess = session.lock().await;

        // Map LoopEvent to AgentTaskEvent
        let mut on_event = |event: LoopEvent<'_, A::Action>| {
            match event {
                LoopEvent::Decision { situation, task } => {
                    handler.on_event(AgentTaskEvent::Decision {
                        situation: situation.to_string(),
                        task: task.to_vec(),
                    });
                }
                LoopEvent::Completed => {
                    handler.on_event(AgentTaskEvent::Completed);
                }
                LoopEvent::ActionStart(action) => {
                    if let Some(path) = A::file_modified(action) {
                        handler.on_event(AgentTaskEvent::FileModified(path));
                    }
                    handler.on_event(AgentTaskEvent::ActionStart(A::action_label(action)));
                }
                LoopEvent::ActionDone(result) => {
                    if let Some(viewable) = A::viewable_content(&result.output) {
                        handler.on_event(AgentTaskEvent::ActionViewable(viewable));
                    } else {
                        handler.on_event(AgentTaskEvent::ActionDone(result.output.clone()));
                    }
                }
                LoopEvent::LoopWarning(n) => {
                    handler.on_event(AgentTaskEvent::Warning(format!(
                        "Loop detected — {} repeats",
                        n
                    )));
                }
                LoopEvent::LoopAbort(n) => {
                    handler.on_event(AgentTaskEvent::Error(format!(
                        "Agent stuck after {} identical actions — aborting",
                        n
                    )));
                }
                LoopEvent::Trimmed(n) => {
                    handler.on_event(AgentTaskEvent::Trimmed(n));
                }
                LoopEvent::MaxStepsReached(n) => {
                    handler.on_event(AgentTaskEvent::Warning(format!(
                        "Max steps ({}) reached",
                        n
                    )));
                }
                LoopEvent::StepStart(_) => {}   // handled above
                LoopEvent::StreamToken(_) => {} // handled above via decide_stream
            }
        };

        if let Some(final_step) = process_step(
            agent,
            &mut sess,
            decision,
            step_num,
            &mut detector,
            &mut on_event,
        )
        .await
        .map_err(|e| format!("{}", e))?
        {
            return Ok(final_step);
        }
    }

    handler.on_event(AgentTaskEvent::Warning(format!(
        "Max steps ({}) reached",
        config.max_steps
    )));
    Ok(config.max_steps)
}
