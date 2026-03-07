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

/// Events emitted by the agent loop for the caller to handle (print, TUI, log).
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
}

/// Configuration for the agent loop.
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

/// SGR Agent trait — implement per project.
///
/// Each BAML project implements this with its own action types,
/// BAML function calls, and tool execution logic.
///
/// The trait uses generic `Future` return types instead of `async fn`
/// to avoid requiring `Send` bounds that conflict with some BAML types.
pub trait SgrAgent {
    /// The action union type (BAML-generated, project-specific).
    type Action;
    /// The message type (implements AgentMessage).
    type Msg: AgentMessage;
    /// Error type.
    type Error: fmt::Display;

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

/// Run the SGR agent loop.
///
/// This is the shared core loop used by all BAML agent projects:
/// `trim → decide → check loop → execute → push results → repeat`
///
/// The `on_event` callback lets the caller react to events (print, TUI update, log)
/// without the loop needing to know about presentation.
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
    F: FnMut(LoopEvent<'_, A::Action>),
{
    let mut detector = LoopDetector::new(config.loop_abort_threshold);

    for step_num in 1..=config.max_steps {
        // Trim context
        let trimmed = session.trim();
        if trimmed > 0 {
            on_event(LoopEvent::Trimmed(trimmed));
        }

        on_event(LoopEvent::StepStart(step_num));

        // LLM decision
        let decision = agent.decide(session.messages()).await?;

        on_event(LoopEvent::Decision {
            state: &decision.state,
            plan: &decision.plan,
        });

        if decision.completed {
            on_event(LoopEvent::Completed);
            return Ok(step_num);
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
                return Ok(step_num);
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
                    // Push tool result to session
                    session.push(
                        <<A::Msg as AgentMessage>::Role>::tool(),
                        result.output.clone(),
                    );
                    let done = result.done;
                    on_event(LoopEvent::ActionDone(&result));
                    if done {
                        return Ok(step_num);
                    }
                }
                Err(e) => {
                    // Push error to session so LLM can recover
                    session.push(
                        <<A::Msg as AgentMessage>::Role>::tool(),
                        format!("Tool error: {}", e),
                    );
                }
            }
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

        assert_eq!(steps, 3); // completes on step 3
        assert!(events.contains(&"completed".to_string()));
        // Session should have tool results
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
}
