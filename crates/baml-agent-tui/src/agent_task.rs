use baml_agent::{LoopDetector, LoopStatus};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Events emitted by the agent task back to the TUI.
///
/// These are agent-side events, not TUI events.
/// The caller maps them to their own `AppEvent` type via the channel.
pub enum AgentTaskEvent {
    /// Streaming text chunk from LLM analysis.
    StreamChunk(String),
    /// Final analysis text (replaces stream).
    Analysis(String),
    /// Plan updates from the step.
    Plan(Vec<String>),
    /// Tool result output.
    ToolResult(String),
    /// A file was modified by a tool action.
    FileModified(String),
    /// Warning (e.g., loop detected).
    Warning(String),
    /// Error message.
    Error(String),
    /// Agent task finished.
    Done,
}

/// Callback trait for handling agent execution events.
///
/// Implement this to map agent events to your TUI's event system.
/// The methods are sync because they typically just send to a channel.
pub trait AgentEventHandler: Send + 'static {
    /// Called for each agent task event. Return false to stop the loop.
    fn on_event(&self, event: AgentTaskEvent) -> bool;
}

/// Simple channel-based handler.
pub struct ChannelHandler<T: Send + 'static> {
    tx: tokio::sync::mpsc::Sender<T>,
    mapper: Box<dyn Fn(AgentTaskEvent) -> T + Send>,
}

impl<T: Send + 'static> ChannelHandler<T> {
    pub fn new(
        tx: tokio::sync::mpsc::Sender<T>,
        mapper: impl Fn(AgentTaskEvent) -> T + Send + 'static,
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

/// Configuration for the agent task.
pub struct AgentTaskConfig {
    /// Max consecutive identical action signatures before abort.
    pub loop_abort_threshold: usize,
}

impl Default for AgentTaskConfig {
    fn default() -> Self {
        Self {
            loop_abort_threshold: 6,
        }
    }
}

/// Spawn an agent loop as a tokio task.
///
/// This is the shared "TUI agent loop" that all projects use:
/// - Streams LLM tokens → `StreamChunk` events
/// - Executes actions → `ToolResult` events
/// - Loop detection via `LoopDetector`
/// - Injects pending user notes between steps
///
/// Returns a `JoinHandle` that the TUI can abort on quit.
///
/// # Type Parameters
/// - `A`: Agent implementing `SgrAgentStream` (LLM calls + tool execution)
/// - `H`: Event handler (typically `ChannelHandler` wrapping mpsc sender)
///
/// # Arguments
/// - `agent`: Shared agent behind `Arc<Mutex>` (for concurrent TUI access)
/// - `pending_notes`: Queue of user messages injected between steps
/// - `handler`: Receives `AgentTaskEvent`s to forward to TUI
/// - `step_stream_fn`: Closure that streams one LLM step. Takes agent ref,
///   returns streaming iterator. This is project-specific because BAML
///   generates different stream types per project.
/// - `is_done_fn`: Checks if an action signals task completion
/// - `file_modified_fn`: Extracts file path if action modifies a file
pub fn spawn_agent_task<A, H, S, SF, D, FM>(
    agent: Arc<Mutex<A>>,
    pending_notes: Arc<Mutex<Vec<String>>>,
    handler: H,
    config: AgentTaskConfig,
    step_stream_fn: S,
    is_done_fn: D,
    file_modified_fn: FM,
) -> tokio::task::JoinHandle<()>
where
    A: Send + 'static,
    H: AgentEventHandler,
    S: Fn(&A) -> SF + Send + 'static,
    SF: std::future::Future<Output = Result<AgentLoopStep<A>, anyhow::Error>> + Send,
    D: Fn(&<A as HasAction>::Action) -> bool + Send + 'static,
    FM: Fn(&<A as HasAction>::Action) -> Option<String> + Send + 'static,
    A: HasAction + HasExecute + HasSession,
{
    tokio::spawn(async move {
        let mut detector = LoopDetector::new(config.loop_abort_threshold);

        loop {
            // Inject queued user notes
            {
                let notes = {
                    let mut q = pending_notes.lock().await;
                    std::mem::take(&mut *q)
                };
                if !notes.is_empty() {
                    let mut locked = agent.lock().await;
                    for note in notes {
                        locked.add_message(
                            "user",
                            &format!("User note while task is running:\n{}", note),
                        );
                        handler.on_event(AgentTaskEvent::Warning(
                            "[NOTE] Queued note injected".to_string(),
                        ));
                    }
                }
            }

            // Trim context
            {
                let mut locked = agent.lock().await;
                locked.trim_session();
            }

            // Stream LLM response
            let step = {
                let locked = agent.lock().await;
                let result = (step_stream_fn)(&*locked).await;
                match result {
                    Ok(step) => step,
                    Err(e) => {
                        handler.on_event(AgentTaskEvent::Error(format!("[ERR] AI Error: {}", e)));
                        break;
                    }
                }
            };

            // Loop detection
            let sig = step.action_signatures.join("|");
            match detector.check(&sig) {
                LoopStatus::Abort(n) => {
                    handler.on_event(AgentTaskEvent::Error(format!(
                        "[ERR] Agent stuck in loop after {} identical actions — aborting",
                        n
                    )));
                    break;
                }
                LoopStatus::Warning(n) => {
                    let mut locked = agent.lock().await;
                    locked.add_message(
                        "user",
                        "SYSTEM: You are repeating the same action. Try a different approach.",
                    );
                    handler.on_event(AgentTaskEvent::Warning(format!(
                        "[WARN] Loop detected — {} repeats",
                        n
                    )));
                }
                LoopStatus::Ok => {}
            }

            // Emit analysis and plan
            handler.on_event(AgentTaskEvent::Plan(step.plan));
            handler.on_event(AgentTaskEvent::Analysis(step.analysis.clone()));

            // Record assistant message
            {
                let mut locked = agent.lock().await;
                locked.add_message(
                    "assistant",
                    &format!("Analysis: {}\nActions: {:?}", step.analysis, step.action_signatures),
                );
            }

            // Execute actions
            let mut is_done = false;
            for (action, _sig) in step.actions.iter().zip(step.action_signatures.iter()) {
                if (is_done_fn)(action) {
                    is_done = true;
                }

                if let Some(path) = (file_modified_fn)(action) {
                    handler.on_event(AgentTaskEvent::FileModified(path));
                }

                let result = {
                    let mut locked = agent.lock().await;
                    locked.execute_action(action).await
                };

                match result {
                    Ok(output) => {
                        let mut locked = agent.lock().await;
                        locked.add_message("user", &format!("Tool result:\n{}", output));
                        handler.on_event(AgentTaskEvent::ToolResult(output));
                    }
                    Err(e) => {
                        let mut locked = agent.lock().await;
                        locked.add_message("user", &format!("Tool error:\n{}", e));
                        handler.on_event(AgentTaskEvent::Error(format!("[ERR] Tool Error\n{}", e)));
                    }
                }
            }

            if is_done {
                break;
            }
        }

        handler.on_event(AgentTaskEvent::Done);
    })
}

/// Intermediate result from one LLM step (after streaming completes).
pub struct AgentLoopStep<A: HasAction> {
    pub analysis: String,
    pub plan: Vec<String>,
    pub actions: Vec<A::Action>,
    pub action_signatures: Vec<String>,
}

/// Trait for agents that have an action type.
pub trait HasAction {
    type Action: Send + Sync;
}

/// Trait for agents that can execute actions.
pub trait HasExecute: HasAction {
    fn execute_action(
        &mut self,
        action: &Self::Action,
    ) -> impl std::future::Future<Output = Result<String, anyhow::Error>> + Send;
}

/// Trait for agents that have a session to manage.
pub trait HasSession {
    fn add_message(&mut self, role: &str, content: &str);
    fn trim_session(&mut self);
}
