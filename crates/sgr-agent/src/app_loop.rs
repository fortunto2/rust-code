use crate::loop_detect::{LoopDetector, LoopStatus, normalize_signature};
use crate::session::{AgentMessage, MessageRole, Session};
use std::fmt;
use std::future::Future;
use tracing::{Instrument, info_span};

/// Result of executing a single action.
pub struct ActionResult {
    /// Text output from the tool (goes into history as tool message).
    pub output: String,
    /// Whether this action signals task completion (e.g. FinishTask, ReportCompletion).
    pub done: bool,
}

/// Result of one LLM decision step (STAR: Situation → Task → Action).
pub struct StepDecision<A> {
    /// S — Situation: current state assessment.
    pub situation: String,
    /// T — Task: remaining steps (first = current).
    pub task: Vec<String>,
    /// Whether the overall task is complete (R — Result).
    pub completed: bool,
    /// A — Action: tools to execute.
    pub actions: Vec<A>,
    /// Soft hints injected as system messages before execution.
    /// Used for intent-mismatch nudges, guardrails, etc.
    pub hints: Vec<String>,
}

impl<A> Default for StepDecision<A> {
    fn default() -> Self {
        Self {
            situation: String::new(),
            task: vec![],
            completed: false,
            actions: vec![],
            hints: vec![],
        }
    }
}

/// Events emitted by the agent loop (print, TUI, log).
pub enum LoopEvent<'a, A> {
    /// Step started (step number, 1-based).
    StepStart(usize),
    /// LLM returned a decision.
    Decision {
        situation: &'a str,
        task: &'a [String],
    },
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

    /// String signature for loop detection (exact match).
    fn action_signature(action: &Self::Action) -> String;

    /// Coarse category for semantic loop detection.
    ///
    /// Default: normalize the signature (strips bash flags, quotes, fallbacks).
    /// Override for project-specific normalization.
    fn action_category(action: &Self::Action) -> String {
        normalize_signature(&Self::action_signature(action))
    }
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
    tracing::info!(
        step = step_num,
        situation = %decision.situation,
        actions = decision.actions.len(),
        completed = decision.completed,
        "agent_decision"
    );

    on_event(LoopEvent::Decision {
        situation: &decision.situation,
        task: &decision.task,
    });

    if decision.completed {
        tracing::info!(step = step_num, "agent_completed");
        // Execute final actions (e.g. FinishTaskTool) so their output is visible,
        // then signal completion.
        for action in &decision.actions {
            on_event(LoopEvent::ActionStart(action));
            match agent.execute(action).await {
                Ok(result) => on_event(LoopEvent::ActionDone(&result)),
                Err(e) => tracing::warn!(error = %e, "final action failed"),
            }
        }
        on_event(LoopEvent::Completed);
        return Ok(Some(step_num));
    }

    // --- Signatures: exact + normalized category ---
    let sig = decision
        .actions
        .iter()
        .map(A::action_signature)
        .collect::<Vec<_>>()
        .join("|");

    let category = decision
        .actions
        .iter()
        .map(A::action_category)
        .collect::<Vec<_>>()
        .join("|");

    // --- Empty actions guard ---
    if decision.actions.is_empty() {
        tracing::warn!(step = step_num, "agent_empty_actions");
        match detector.check(&sig) {
            LoopStatus::Abort(n) => {
                tracing::error!(step = step_num, repeats = n, "agent_empty_abort");
                on_event(LoopEvent::LoopAbort(n));
                session.push(
                    <<A::Msg as AgentMessage>::Role>::system(),
                    "SYSTEM: Repeatedly returning empty actions. Session terminated.".into(),
                );
                return Ok(Some(step_num));
            }
            _ => {
                session.push(
                    <<A::Msg as AgentMessage>::Role>::system(),
                    "SYSTEM: You returned empty next_actions. You MUST emit at least one tool call \
                     in next_actions array. Look at the TOOLS section and pick the right tool for \
                     your current phase.".into(),
                );
                return Ok(None);
            }
        }
    }

    // --- Tier 1+2: exact + category loop detection ---
    match detector.check_with_category(&sig, &category) {
        LoopStatus::Abort(n) => {
            tracing::error!(step = step_num, repeats = n, category = %category, "agent_loop_abort");
            on_event(LoopEvent::LoopAbort(n));
            session.push(
                <<A::Msg as AgentMessage>::Role>::system(),
                format!(
                    "SYSTEM: Detected {} repetitions of the same action (category: {}). \
                     The result will not change. Session terminated.",
                    n, category
                ),
            );
            return Ok(Some(step_num));
        }
        LoopStatus::Warning(n) => {
            tracing::warn!(step = step_num, repeats = n, category = %category, "agent_loop_warning");
            on_event(LoopEvent::LoopWarning(n));
            // Use tool role — models on FC mode see tool results better than system messages
            session.push(
                <<A::Msg as AgentMessage>::Role>::tool(),
                format!(
                    "⚠ LOOP WARNING: You repeated the same action {} times (category: {}). \
                     The result is DEFINITIVE and will NOT change. Do NOT retry. \
                     Proceed to the NEXT step in your plan or use finish to complete.",
                    n, category
                ),
            );
        }
        LoopStatus::Ok => {}
    }

    // --- Inject hints as tool messages (better visibility in FC mode) ---
    for hint in &decision.hints {
        session.push(
            <<A::Msg as AgentMessage>::Role>::tool(),
            format!("HINT: {}", hint),
        );
    }

    // --- Execute actions ---
    for action in &decision.actions {
        let action_sig = A::action_signature(action);
        on_event(LoopEvent::ActionStart(action));

        let t0 = std::time::Instant::now();
        match agent.execute(action).await {
            Ok(result) => {
                let elapsed_ms = t0.elapsed().as_millis() as u64;
                tracing::info!(
                    step = step_num,
                    action = %action_sig,
                    duration_ms = elapsed_ms,
                    output_bytes = result.output.len(),
                    done = result.done,
                    "tool_executed"
                );

                session.push(
                    <<A::Msg as AgentMessage>::Role>::tool(),
                    result.output.clone(),
                );

                let done = result.done;
                on_event(LoopEvent::ActionDone(&result));

                // --- Tier 3: output stagnation ---
                match detector.record_output(&result.output) {
                    LoopStatus::Abort(n) => {
                        tracing::error!(step = step_num, repeats = n, "output_stagnation_abort");
                        on_event(LoopEvent::LoopAbort(n));
                        session.push(
                            <<A::Msg as AgentMessage>::Role>::system(),
                            format!(
                                "SYSTEM: Tool returned identical output {} times. The result is \
                                 DEFINITIVE and will not change. If searching found nothing, \
                                 nothing exists. Accept the result and proceed to the next task \
                                 step or use FinishTaskTool.",
                                n
                            ),
                        );
                        return Ok(Some(step_num));
                    }
                    LoopStatus::Warning(n) => {
                        on_event(LoopEvent::LoopWarning(n));
                        session.push(
                            <<A::Msg as AgentMessage>::Role>::tool(),
                            format!(
                                "⚠ STAGNATION: Same tool output {} times. The result will NOT \
                                 change. Do NOT retry — move to the NEXT step or use finish.",
                                n
                            ),
                        );
                    }
                    LoopStatus::Ok => {}
                }

                if done {
                    return Ok(Some(step_num));
                }
            }
            Err(e) => {
                tracing::error!(step = step_num, action = %action_sig, error = %e, "tool_error");
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
    tracing::info!(max_steps = config.max_steps, "agent_loop_start");

    for step_num in 1..=config.max_steps {
        let trimmed = session.trim();
        if trimmed > 0 {
            tracing::info!(trimmed, "context_trimmed");
            on_event(LoopEvent::Trimmed(trimmed));
        }

        on_event(LoopEvent::StepStart(step_num));

        let step_span = info_span!("agent_step", step = step_num);
        let t0 = std::time::Instant::now();
        let decision = agent
            .decide(session.messages())
            .instrument(step_span)
            .await?;
        let decide_ms = t0.elapsed().as_millis() as u64;
        tracing::info!(step = step_num, decide_ms, "llm_decision");

        if let Some(final_step) = process_step(
            agent,
            session,
            decision,
            step_num,
            &mut detector,
            &mut on_event,
        )
        .await?
        {
            tracing::info!(total_steps = final_step, "agent_loop_done");
            return Ok(final_step);
        }
    }

    tracing::warn!(max_steps = config.max_steps, "agent_max_steps_reached");
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
    tracing::info!(
        max_steps = config.max_steps,
        streaming = true,
        "agent_loop_start"
    );

    for step_num in 1..=config.max_steps {
        let trimmed = session.trim();
        if trimmed > 0 {
            tracing::info!(trimmed, "context_trimmed");
            on_event(LoopEvent::Trimmed(trimmed));
        }

        on_event(LoopEvent::StepStart(step_num));

        let step_span = info_span!("agent_step", step = step_num);
        let t0 = std::time::Instant::now();
        let decision = agent
            .decide_stream(session.messages(), |token| {
                on_event(LoopEvent::StreamToken(token));
            })
            .instrument(step_span)
            .await?;
        let decide_ms = t0.elapsed().as_millis() as u64;
        tracing::info!(step = step_num, decide_ms, "llm_decision");

        if let Some(final_step) = process_step(
            agent,
            session,
            decision,
            step_num,
            &mut detector,
            &mut on_event,
        )
        .await?
        {
            tracing::info!(total_steps = final_step, "agent_loop_done");
            return Ok(final_step);
        }
    }

    tracing::warn!(max_steps = config.max_steps, "agent_max_steps_reached");
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
                    situation: "done".into(),
                    completed: true,
                    ..Default::default()
                })
            } else {
                Ok(StepDecision {
                    situation: format!("{} steps left", remaining - 1),
                    task: vec!["do something".into()],
                    actions: vec![format!("action_{}", remaining)],
                    ..Default::default()
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
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(TestRole::User, "do something".into());

        let agent = MockAgent {
            steps_before_done: AtomicUsize::new(3),
        };
        let config = LoopConfig {
            max_steps: 10,
            loop_abort_threshold: 6,
        };

        let mut events = vec![];
        let steps = run_loop(&agent, &mut session, &config, |event| match &event {
            LoopEvent::StepStart(n) => events.push(format!("step:{}", n)),
            LoopEvent::Completed => events.push("completed".into()),
            LoopEvent::ActionDone(r) => events.push(format!("done:{}", r.output)),
            _ => {}
        })
        .await
        .unwrap();

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
                situation: "stuck".into(),
                task: vec!["same thing again".into()],
                actions: vec!["same_action".into()],
                ..Default::default()
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
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(TestRole::User, "do something".into());

        let config = LoopConfig {
            max_steps: 20,
            loop_abort_threshold: 4,
        };

        let mut got_warning = false;
        let mut got_abort = false;
        let steps = run_loop(&LoopyAgent, &mut session, &config, |event| match event {
            LoopEvent::LoopWarning(_) => got_warning = true,
            LoopEvent::LoopAbort(_) => got_abort = true,
            _ => {}
        })
        .await
        .unwrap();

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
                situation: "done".into(),
                completed: true,
                ..Default::default()
            })
        }

        async fn execute(&self, _action: &String) -> Result<ActionResult, String> {
            Ok(ActionResult {
                output: "ok".into(),
                done: false,
            })
        }

        fn action_signature(action: &String) -> String {
            action.clone()
        }
    }

    impl SgrAgentStream for StreamingAgent {
        #[allow(clippy::manual_async_fn)]
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
                    situation: "done".into(),
                    completed: true,
                    ..Default::default()
                })
            }
        }
    }

    #[tokio::test]
    async fn streaming_tokens_emitted() {
        let dir = std::env::temp_dir().join("baml_loop_test_stream");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(TestRole::User, "hello".into());

        let config = LoopConfig {
            max_steps: 5,
            loop_abort_threshold: 6,
        };

        let mut tokens = vec![];
        let mut completed = false;
        run_loop_stream(
            &StreamingAgent,
            &mut session,
            &config,
            |event| match event {
                LoopEvent::StreamToken(t) => tokens.push(t.to_string()),
                LoopEvent::Completed => completed = true,
                _ => {}
            },
        )
        .await
        .unwrap();

        assert!(completed);
        assert_eq!(tokens, vec!["Thin", "king", "..."]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Empty actions guard test ---

    struct EmptyActionsAgent {
        call_count: AtomicUsize,
    }

    impl SgrAgent for EmptyActionsAgent {
        type Action = String;
        type Msg = TestMsg;
        type Error = String;

        async fn decide(&self, _messages: &[TestMsg]) -> Result<StepDecision<String>, String> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n < 2 {
                // First 2 calls: empty actions (model forgot tool calls)
                Ok(StepDecision {
                    situation: "thinking...".into(),
                    task: vec!["do something".into()],
                    ..Default::default()
                })
            } else {
                // After nudge, model recovers
                Ok(StepDecision {
                    situation: "done".into(),
                    completed: true,
                    ..Default::default()
                })
            }
        }

        async fn execute(&self, _action: &String) -> Result<ActionResult, String> {
            Ok(ActionResult {
                output: "ok".into(),
                done: false,
            })
        }

        fn action_signature(action: &String) -> String {
            action.clone()
        }
    }

    #[tokio::test]
    async fn empty_actions_nudges_model() {
        let dir = std::env::temp_dir().join("baml_loop_test_empty_actions");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(TestRole::User, "do something".into());

        let agent = EmptyActionsAgent {
            call_count: AtomicUsize::new(0),
        };
        let config = LoopConfig {
            max_steps: 10,
            loop_abort_threshold: 6,
        };

        let mut completed = false;
        let steps = run_loop(&agent, &mut session, &config, |event| {
            if matches!(event, LoopEvent::Completed) {
                completed = true;
            }
        })
        .await
        .unwrap();

        assert!(completed, "agent should recover after nudge");
        // 2 empty steps + 1 completed = 3 decide calls, but empty steps return Ok(None)
        // so step counter advances: step 1 (empty), step 2 (empty), step 3 (completed)
        assert_eq!(steps, 3);

        // Session should contain nudge messages
        let messages: Vec<&str> = session.messages().iter().map(|m| m.content()).collect();
        let nudges = messages
            .iter()
            .filter(|m| m.contains("empty next_actions"))
            .count();
        assert_eq!(
            nudges, 2,
            "should have 2 nudge messages for 2 empty action steps"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn empty_actions_aborts_after_threshold() {
        let dir = std::env::temp_dir().join("baml_loop_test_empty_abort");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(TestRole::User, "do something".into());

        // Agent that always returns empty actions — never recovers
        // Set threshold low so it aborts quickly
        let config = LoopConfig {
            max_steps: 20,
            loop_abort_threshold: 4,
        };

        // Use a separate agent that never completes
        struct NeverRecoverAgent;
        impl SgrAgent for NeverRecoverAgent {
            type Action = String;
            type Msg = TestMsg;
            type Error = String;
            async fn decide(&self, _messages: &[TestMsg]) -> Result<StepDecision<String>, String> {
                Ok(StepDecision {
                    situation: "stuck".into(),
                    task: vec!["try again".into()],
                    ..Default::default()
                })
            }
            async fn execute(&self, _action: &String) -> Result<ActionResult, String> {
                Ok(ActionResult {
                    output: "ok".into(),
                    done: false,
                })
            }
            fn action_signature(action: &String) -> String {
                action.clone()
            }
        }

        let mut got_abort = false;
        let _steps = run_loop(&NeverRecoverAgent, &mut session, &config, |event| {
            if matches!(event, LoopEvent::LoopAbort(_)) {
                got_abort = true;
            }
        })
        .await
        .unwrap();

        assert!(got_abort, "should abort after repeated empty actions");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Non-streaming agent can also use run_loop (base trait only).
    #[tokio::test]
    async fn non_streaming_agent_works() {
        let dir = std::env::temp_dir().join("baml_loop_test_nostream");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(TestRole::User, "hello".into());

        let config = LoopConfig {
            max_steps: 5,
            loop_abort_threshold: 6,
        };

        // StreamingAgent also implements SgrAgent, so run_loop works
        let mut completed = false;
        run_loop(&StreamingAgent, &mut session, &config, |event| {
            if matches!(event, LoopEvent::Completed) {
                completed = true;
            }
        })
        .await
        .unwrap();

        assert!(completed);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
