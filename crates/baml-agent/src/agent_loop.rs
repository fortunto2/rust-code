use crate::loop_detect::{LoopDetector, LoopStatus};
use crate::session::{AgentMessage, MessageRole, Session};
use std::fmt;
use std::future::Future;

/// Result of executing a single action.
pub struct ActionResult {
    /// Text output from the tool (goes into history as tool message).
    pub output: String,
    /// Whether this action signals task completion (e.g. FinishTask, ReportCompletion).
    pub done: bool,
}

/// Result of one LLM decision step.
pub struct StepDecision<A> {
    /// Current state description (shown to user).
    pub state: String,
    /// Remaining plan steps (shown to user).
    pub plan: Vec<String>,
    /// Whether the overall task is complete.
    pub completed: bool,
    /// Actions to execute this step.
    pub actions: Vec<A>,
}

/// Events emitted by the agent loop (print, TUI, log).
pub enum LoopEvent<'a, A> {
    /// Step started (step number, 1-based).
    StepStart(usize),
    /// LLM returned a decision.
    Decision { state: &'a str, plan: &'a [String] },
    /// Task completed by LLM (task_completed=true).
    Completed,
    /// About to execute an action.
    ActionStart(&'a A),
    /// Action executed, result available.
    ActionDone(&'a ActionResult),
    /// Loop warning (repeated actions).
    LoopWarning(usize),
    /// Loop abort (too many repeats).
    LoopAbort(usize),
    /// Context trimmed.
    Trimmed(usize),
    /// Max steps reached.
    MaxStepsReached(usize),
    /// Streaming token from LLM (only emitted by `run_loop_stream`).
    StreamToken(&'a str),
}

/// Configuration for the agent loop.
#[derive(Clone)]
pub struct LoopConfig {
    pub max_steps: usize,
    pub loop_abort_threshold: usize,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_steps: 50,
            loop_abort_threshold: 6,
        }
    }
}

/// Base SGR Agent trait — implement per project.
///
/// Covers the non-streaming case (va-agent, simple CLIs).
/// For streaming, implement [`SgrAgentStream`] on top.
///
/// # Stateful executors
///
/// If `execute` needs mutable state (e.g. MCP connections), use interior
/// mutability (`Mutex`, `RwLock`). The trait takes `&self` to allow
/// concurrent action execution in the future.
pub trait SgrAgent {
    /// The action union type (BAML-generated, project-specific).
    type Action: Send + Sync;
    /// The message type (implements AgentMessage).
    type Msg: AgentMessage + Send + Sync;
    /// Error type.
    type Error: fmt::Display + Send;

    /// Call LLM to decide next actions.
    fn decide(
        &self,
        messages: &[Self::Msg],
    ) -> impl Future<Output = Result<StepDecision<Self::Action>, Self::Error>> + Send;

    /// Execute a single action. Returns tool output + done flag.
    /// Does NOT push to session — the loop handles that.
    fn execute(
        &self,
        action: &Self::Action,
    ) -> impl Future<Output = Result<ActionResult, Self::Error>> + Send;

    /// String signature for loop detection.
    fn action_signature(action: &Self::Action) -> String;
}

/// Streaming extension for SGR agents.
///
/// Implement this alongside [`SgrAgent`] to get streaming tokens
/// during the decision phase. Use with [`run_loop_stream`].
///
/// ```ignore
/// impl SgrAgentStream for MyAgent {
///     fn decide_stream<T>(&self, messages: &[Msg], mut on_token: T)
///         -> impl Future<Output = Result<StepDecision<Action>, Error>> + Send
///     where T: FnMut(&str) + Send
///     {
///         async move {
///             let stream = B.MyFunction.stream(&messages).await?;
///             while let Some(partial) = stream.next().await {
///                 on_token(&partial.raw_text);
///             }
///             let result = stream.get_final_response().await?;
///             Ok(StepDecision { ... })
///         }
///     }
/// }
/// ```
pub trait SgrAgentStream: SgrAgent {
    /// Call LLM with streaming — emits tokens via `on_token` callback.
    fn decide_stream<T>(
        &self,
        messages: &[Self::Msg],
        on_token: T,
    ) -> impl Future<Output = Result<StepDecision<Self::Action>, Self::Error>> + Send
    where
        T: FnMut(&str) + Send;
}

// --- Shared loop internals ---

/// Post-decision processing: loop detection, action execution, session updates.
///
/// Shared between `run_loop`, `run_loop_stream`, and custom loops (e.g. TUI).
/// Returns `Some(step_num)` if the loop should stop, `None` to continue.
pub async fn process_step<A, F>(
    agent: &A,
    session: &mut Session<A::Msg>,
    decision: StepDecision<A::Action>,
    step_num: usize,
    detector: &mut LoopDetector,
    on_event: &mut F,
) -> Result<Option<usize>, A::Error>
where
    A: SgrAgent,
    F: FnMut(LoopEvent<'_, A::Action>) + Send,
{
    on_event(LoopEvent::Decision {
        state: &decision.state,
        plan: &decision.plan,
    });

    if decision.completed {
        on_event(LoopEvent::Completed);
        return Ok(Some(step_num));
    }

    // Loop detection
    let sig = decision
        .actions
        .iter()
        .map(A::action_signature)
        .collect::<Vec<_>>()
        .join("|");

    match detector.check(&sig) {
        LoopStatus::Abort(n) => {
            on_event(LoopEvent::LoopAbort(n));
            session.push(
                <<A::Msg as AgentMessage>::Role>::system(),
                "SYSTEM: You have been repeating the same action. Session terminated.".into(),
            );
            return Ok(Some(step_num));
        }
        LoopStatus::Warning(n) => {
            on_event(LoopEvent::LoopWarning(n));
            session.push(
                <<A::Msg as AgentMessage>::Role>::system(),
                "SYSTEM: You are repeating the same action. Try a different approach or report completion.".into(),
            );
        }
        LoopStatus::Ok => {}
    }

    // Execute actions
    for action in &decision.actions {
        on_event(LoopEvent::ActionStart(action));

        match agent.execute(action).await {
            Ok(result) => {
                session.push(
                    <<A::Msg as AgentMessage>::Role>::tool(),
                    result.output.clone(),
                );
                let done = result.done;
                on_event(LoopEvent::ActionDone(&result));
                if done {
                    return Ok(Some(step_num));
                }
            }
            Err(e) => {
                session.push(
                    <<A::Msg as AgentMessage>::Role>::tool(),
                    format!("Tool error: {}", e),
                );
            }
        }
    }

    Ok(None) // continue
}

/// Run the SGR agent loop (non-streaming).
///
/// `trim → decide → check loop → execute → push results → repeat`
///
/// Returns the number of steps executed.
pub async fn run_loop<A, F>(
    agent: &A,
    session: &mut Session<A::Msg>,
    config: &LoopConfig,
    mut on_event: F,
) -> Result<usize, A::Error>
where
    A: SgrAgent,
    F: FnMut(LoopEvent<'_, A::Action>) + Send,
{
    let mut detector = LoopDetector::new(config.loop_abort_threshold);

    for step_num in 1..=config.max_steps {
        let trimmed = session.trim();
        if trimmed > 0 {
            on_event(LoopEvent::Trimmed(trimmed));
        }

        on_event(LoopEvent::StepStart(step_num));

        let decision = agent.decide(session.messages()).await?;

        if let Some(final_step) = process_step(agent, session, decision, step_num, &mut detector, &mut on_event).await? {
            return Ok(final_step);
        }
    }

    on_event(LoopEvent::MaxStepsReached(config.max_steps));
    Ok(config.max_steps)
}

/// Run the SGR agent loop with streaming tokens.
///
/// Same as [`run_loop`] but calls `decide_stream` instead of `decide`,
/// emitting `LoopEvent::StreamToken` during the decision phase.
///
/// Requires the agent to implement [`SgrAgentStream`].
pub async fn run_loop_stream<A, F>(
    agent: &A,
    session: &mut Session<A::Msg>,
    config: &LoopConfig,
    mut on_event: F,
) -> Result<usize, A::Error>
where
    A: SgrAgentStream,
    F: FnMut(LoopEvent<'_, A::Action>) + Send,
{
    let mut detector = LoopDetector::new(config.loop_abort_threshold);

    for step_num in 1..=config.max_steps {
        let trimmed = session.trim();
        if trimmed > 0 {
            on_event(LoopEvent::Trimmed(trimmed));
        }

        on_event(LoopEvent::StepStart(step_num));

        let decision = agent.decide_stream(session.messages(), |token| {
            on_event(LoopEvent::StreamToken(token));
        }).await?;

        if let Some(final_step) = process_step(agent, session, decision, step_num, &mut detector, &mut on_event).await? {
            return Ok(final_step);
        }
    }

    on_event(LoopEvent::MaxStepsReached(config.max_steps));
    Ok(config.max_steps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::{TestMsg, TestRole};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockAgent {
        steps_before_done: AtomicUsize,
    }

    impl SgrAgent for MockAgent {
        type Action = String;
        type Msg = TestMsg;
        type Error = String;

        async fn decide(&self, _messages: &[TestMsg]) -> Result<StepDecision<String>, String> {
            let remaining = self.steps_before_done.fetch_sub(1, Ordering::SeqCst);
            if remaining <= 1 {
                Ok(StepDecision {
                    state: "done".into(),
                    plan: vec![],
                    completed: true,
                    actions: vec![],
                })
            } else {
                Ok(StepDecision {
                    state: format!("{} steps left", remaining - 1),
                    plan: vec!["do something".into()],
                    completed: false,
                    actions: vec![format!("action_{}", remaining)],
                })
            }
        }

        async fn execute(&self, action: &String) -> Result<ActionResult, String> {
            Ok(ActionResult {
                output: format!("result of {}", action),
                done: false,
            })
        }

        fn action_signature(action: &String) -> String {
            action.clone()
        }
    }

    #[tokio::test]
    async fn loop_completes_after_n_steps() {
        let dir = std::env::temp_dir().join("baml_loop_test_complete");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "do something".into());

        let agent = MockAgent {
            steps_before_done: AtomicUsize::new(3),
        };
        let config = LoopConfig { max_steps: 10, loop_abort_threshold: 6 };

        let mut events = vec![];
        let steps = run_loop(&agent, &mut session, &config, |event| {
            match &event {
                LoopEvent::StepStart(n) => events.push(format!("step:{}", n)),
                LoopEvent::Completed => events.push("completed".into()),
                LoopEvent::ActionDone(r) => events.push(format!("done:{}", r.output)),
                _ => {}
            }
        }).await.unwrap();

        assert_eq!(steps, 3);
        assert!(events.contains(&"completed".to_string()));
        assert!(session.len() > 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    struct LoopyAgent;

    impl SgrAgent for LoopyAgent {
        type Action = String;
        type Msg = TestMsg;
        type Error = String;

        async fn decide(&self, _messages: &[TestMsg]) -> Result<StepDecision<String>, String> {
            Ok(StepDecision {
                state: "stuck".into(),
                plan: vec!["same thing again".into()],
                completed: false,
                actions: vec!["same_action".into()],
            })
        }

        async fn execute(&self, _action: &String) -> Result<ActionResult, String> {
            Ok(ActionResult {
                output: "same result".into(),
                done: false,
            })
        }

        fn action_signature(action: &String) -> String {
            action.clone()
        }
    }

    #[tokio::test]
    async fn loop_detects_and_aborts() {
        let dir = std::env::temp_dir().join("baml_loop_test_abort");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "do something".into());

        let config = LoopConfig { max_steps: 20, loop_abort_threshold: 4 };

        let mut got_warning = false;
        let mut got_abort = false;
        let steps = run_loop(&LoopyAgent, &mut session, &config, |event| {
            match event {
                LoopEvent::LoopWarning(_) => got_warning = true,
                LoopEvent::LoopAbort(_) => got_abort = true,
                _ => {}
            }
        }).await.unwrap();

        assert!(got_warning);
        assert!(got_abort);
        assert!(steps <= 4);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Streaming trait test ---

    struct StreamingAgent;

    impl SgrAgent for StreamingAgent {
        type Action = String;
        type Msg = TestMsg;
        type Error = String;

        async fn decide(&self, _messages: &[TestMsg]) -> Result<StepDecision<String>, String> {
            Ok(StepDecision {
                state: "done".into(),
                plan: vec![],
                completed: true,
                actions: vec![],
            })
        }

        async fn execute(&self, _action: &String) -> Result<ActionResult, String> {
            Ok(ActionResult { output: "ok".into(), done: false })
        }

        fn action_signature(action: &String) -> String {
            action.clone()
        }
    }

    impl SgrAgentStream for StreamingAgent {
        fn decide_stream<T>(
            &self,
            _messages: &[TestMsg],
            mut on_token: T,
        ) -> impl Future<Output = Result<StepDecision<String>, String>> + Send
        where
            T: FnMut(&str) + Send,
        {
            async move {
                on_token("Thin");
                on_token("king");
                on_token("...");
                Ok(StepDecision {
                    state: "done".into(),
                    plan: vec![],
                    completed: true,
                    actions: vec![],
                })
            }
        }
    }

    #[tokio::test]
    async fn streaming_tokens_emitted() {
        let dir = std::env::temp_dir().join("baml_loop_test_stream");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "hello".into());

        let config = LoopConfig { max_steps: 5, loop_abort_threshold: 6 };

        let mut tokens = vec![];
        let mut completed = false;
        run_loop_stream(&StreamingAgent, &mut session, &config, |event| {
            match event {
                LoopEvent::StreamToken(t) => tokens.push(t.to_string()),
                LoopEvent::Completed => completed = true,
                _ => {}
            }
        }).await.unwrap();

        assert!(completed);
        assert_eq!(tokens, vec!["Thin", "king", "..."]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Non-streaming agent can also use run_loop (base trait only).
    #[tokio::test]
    async fn non_streaming_agent_works() {
        let dir = std::env::temp_dir().join("baml_loop_test_nostream");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "hello".into());

        let config = LoopConfig { max_steps: 5, loop_abort_threshold: 6 };

        // StreamingAgent also implements SgrAgent, so run_loop works
        let mut completed = false;
        run_loop(&StreamingAgent, &mut session, &config, |event| {
            if matches!(event, LoopEvent::Completed) { completed = true; }
        }).await.unwrap();

        assert!(completed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
