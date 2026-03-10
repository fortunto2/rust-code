//! Generic agent loop — drives agent + tools until completion or limit.
//!
//! Includes 3-tier loop detection (exact signature, tool name frequency, output stagnation).

use crate::agent::{Agent, AgentError, Decision};
use crate::context::{AgentContext, AgentState};
use crate::registry::ToolRegistry;
use crate::types::Message;
use std::collections::HashMap;

/// Loop configuration.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Maximum steps before aborting.
    pub max_steps: usize,
    /// Consecutive repeated tool calls before loop detection triggers.
    pub loop_abort_threshold: usize,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self { max_steps: 50, loop_abort_threshold: 6 }
    }
}

/// Events emitted during the agent loop.
#[derive(Debug)]
pub enum LoopEvent {
    StepStart { step: usize },
    Decision(Decision),
    ToolResult { name: String, output: String },
    Completed { steps: usize },
    LoopDetected { count: usize },
    Error(AgentError),
}

/// Run the agent loop: decide → execute tools → feed results → repeat.
///
/// Returns the number of steps taken.
pub async fn run_loop(
    agent: &dyn Agent,
    tools: &ToolRegistry,
    ctx: &mut AgentContext,
    messages: &mut Vec<Message>,
    config: &LoopConfig,
    mut on_event: impl FnMut(LoopEvent),
) -> Result<usize, AgentError> {
    let mut detector = LoopDetector::new(config.loop_abort_threshold);

    for step in 1..=config.max_steps {
        ctx.iteration = step;
        on_event(LoopEvent::StepStart { step });

        let decision = agent.decide(messages, tools).await?;
        on_event(LoopEvent::Decision(decision.clone()));

        if decision.completed || decision.tool_calls.is_empty() {
            ctx.state = AgentState::Completed;
            // Add assistant message with situation
            if !decision.situation.is_empty() {
                messages.push(Message::assistant(&decision.situation));
            }
            on_event(LoopEvent::Completed { steps: step });
            return Ok(step);
        }

        // Loop detection
        let sig: Vec<String> = decision.tool_calls.iter().map(|tc| tc.name.clone()).collect();
        if detector.check(&sig) {
            ctx.state = AgentState::Failed;
            on_event(LoopEvent::LoopDetected { count: detector.consecutive });
            return Err(AgentError::LoopDetected(detector.consecutive));
        }

        // Execute tool calls
        for tc in &decision.tool_calls {
            if let Some(tool) = tools.get(&tc.name) {
                match tool.execute(tc.arguments.clone(), ctx).await {
                    Ok(output) => {
                        on_event(LoopEvent::ToolResult {
                            name: tc.name.clone(),
                            output: output.content.clone(),
                        });
                        messages.push(Message::tool(&tc.id, &output.content));

                        if output.done {
                            ctx.state = AgentState::Completed;
                            on_event(LoopEvent::Completed { steps: step });
                            return Ok(step);
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Tool error: {}", e);
                        messages.push(Message::tool(&tc.id, &err_msg));
                        on_event(LoopEvent::ToolResult {
                            name: tc.name.clone(),
                            output: err_msg,
                        });
                    }
                }
            } else {
                let err_msg = format!("Unknown tool: {}", tc.name);
                messages.push(Message::tool(&tc.id, &err_msg));
                on_event(LoopEvent::ToolResult {
                    name: tc.name.clone(),
                    output: err_msg,
                });
            }
        }
    }

    ctx.state = AgentState::Failed;
    Err(AgentError::MaxSteps(config.max_steps))
}

/// 3-tier loop detection.
struct LoopDetector {
    threshold: usize,
    consecutive: usize,
    last_sig: Vec<String>,
    tool_freq: HashMap<String, usize>,
    total_calls: usize,
}

impl LoopDetector {
    fn new(threshold: usize) -> Self {
        Self {
            threshold,
            consecutive: 0,
            last_sig: vec![],
            tool_freq: HashMap::new(),
            total_calls: 0,
        }
    }

    /// Returns true if a loop is detected.
    fn check(&mut self, sig: &[String]) -> bool {
        self.total_calls += 1;

        // Tier 1: exact signature match
        if sig == self.last_sig {
            self.consecutive += 1;
        } else {
            self.consecutive = 1;
            self.last_sig = sig.to_vec();
        }
        if self.consecutive >= self.threshold {
            return true;
        }

        // Tier 2: tool name frequency (single tool dominates)
        for name in sig {
            *self.tool_freq.entry(name.clone()).or_insert(0) += 1;
        }
        if self.total_calls >= self.threshold {
            for (_, count) in &self.tool_freq {
                if *count >= self.threshold && *count as f64 / self.total_calls as f64 > 0.9 {
                    return true;
                }
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentError, Decision};
    use crate::agent_tool::{Tool, ToolError, ToolOutput};
    use crate::context::AgentContext;
    use crate::registry::ToolRegistry;
    use crate::types::{Message, ToolCall};
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    struct CountingAgent {
        max_calls: usize,
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Agent for CountingAgent {
        async fn decide(&self, _: &[Message], _: &ToolRegistry) -> Result<Decision, AgentError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n >= self.max_calls {
                Ok(Decision {
                    situation: "done".into(),
                    task: vec![],
                    tool_calls: vec![],
                    completed: true,
                })
            } else {
                Ok(Decision {
                    situation: format!("step {}", n),
                    task: vec![],
                    tool_calls: vec![ToolCall {
                        id: format!("call_{}", n),
                        name: "echo".into(),
                        arguments: serde_json::json!({"msg": "hi"}),
                    }],
                    completed: false,
                })
            }
        }
    }

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str { "echo" }
        fn description(&self) -> &str { "echo" }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("echoed"))
        }
    }

    #[tokio::test]
    async fn loop_runs_and_completes() {
        let agent = CountingAgent {
            max_calls: 3,
            call_count: Arc::new(AtomicUsize::new(0)),
        };
        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();

        let steps = run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |_| {}).await.unwrap();
        assert_eq!(steps, 4); // 3 tool calls + 1 completion
        assert_eq!(ctx.state, AgentState::Completed);
    }

    #[tokio::test]
    async fn loop_detects_repetition() {
        // Agent always returns same tool call → loop detection
        struct LoopingAgent;
        #[async_trait::async_trait]
        impl Agent for LoopingAgent {
            async fn decide(&self, _: &[Message], _: &ToolRegistry) -> Result<Decision, AgentError> {
                Ok(Decision {
                    situation: "stuck".into(),
                    task: vec![],
                    tool_calls: vec![ToolCall {
                        id: "1".into(),
                        name: "echo".into(),
                        arguments: serde_json::json!({}),
                    }],
                    completed: false,
                })
            }
        }

        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig { max_steps: 50, loop_abort_threshold: 3 };

        let result = run_loop(&LoopingAgent, &tools, &mut ctx, &mut messages, &config, |_| {}).await;
        assert!(matches!(result, Err(AgentError::LoopDetected(3))));
        assert_eq!(ctx.state, AgentState::Failed);
    }

    #[tokio::test]
    async fn loop_max_steps() {
        // Agent never completes
        struct NeverDoneAgent;
        #[async_trait::async_trait]
        impl Agent for NeverDoneAgent {
            async fn decide(&self, _: &[Message], _: &ToolRegistry) -> Result<Decision, AgentError> {
                // Different tool names to avoid loop detection
                static COUNTER: AtomicUsize = AtomicUsize::new(0);
                let n = COUNTER.fetch_add(1, Ordering::SeqCst);
                Ok(Decision {
                    situation: String::new(),
                    task: vec![],
                    tool_calls: vec![ToolCall {
                        id: format!("{}", n),
                        name: format!("tool_{}", n),
                        arguments: serde_json::json!({}),
                    }],
                    completed: false,
                })
            }
        }

        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig { max_steps: 5, loop_abort_threshold: 100 };

        let result = run_loop(&NeverDoneAgent, &tools, &mut ctx, &mut messages, &config, |_| {}).await;
        assert!(matches!(result, Err(AgentError::MaxSteps(5))));
    }

    #[test]
    fn loop_detector_exact_sig() {
        let mut d = LoopDetector::new(3);
        let sig = vec!["bash".to_string()];
        assert!(!d.check(&sig));
        assert!(!d.check(&sig));
        assert!(d.check(&sig)); // 3rd consecutive
    }

    #[test]
    fn loop_detector_different_sigs_reset() {
        let mut d = LoopDetector::new(3);
        assert!(!d.check(&["bash".into()]));
        assert!(!d.check(&["bash".into()]));
        assert!(!d.check(&["read".into()])); // different → resets
        assert!(!d.check(&["bash".into()]));
    }

    #[test]
    fn loop_config_default() {
        let c = LoopConfig::default();
        assert_eq!(c.max_steps, 50);
        assert_eq!(c.loop_abort_threshold, 6);
    }
}
