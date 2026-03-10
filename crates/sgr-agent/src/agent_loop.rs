//! Generic agent loop — drives agent + tools until completion or limit.
//!
//! Includes 3-tier loop detection (exact signature, tool name frequency, output stagnation).

use crate::agent::{Agent, AgentError, Decision};
use crate::context::{AgentContext, AgentState};
use crate::registry::ToolRegistry;
use crate::types::{Message, SgrError};
use futures::future::join_all;
use std::collections::HashMap;

/// Max consecutive parsing errors before aborting the loop.
const MAX_PARSE_RETRIES: usize = 3;

/// Check if an agent error is recoverable (parsing/empty response).
fn is_recoverable_error(e: &AgentError) -> bool {
    matches!(
        e,
        AgentError::Llm(SgrError::Json(_))
            | AgentError::Llm(SgrError::EmptyResponse)
            | AgentError::Llm(SgrError::Schema(_))
    )
}

/// Loop configuration.
#[derive(Debug, Clone)]
pub struct LoopConfig {
    /// Maximum steps before aborting.
    pub max_steps: usize,
    /// Consecutive repeated tool calls before loop detection triggers.
    pub loop_abort_threshold: usize,
    /// Max messages to keep in context (0 = unlimited).
    /// Keeps first 2 (system + user prompt) + last N messages.
    pub max_messages: usize,
    /// Auto-complete if agent returns same situation text N times.
    pub auto_complete_threshold: usize,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            max_steps: 50,
            loop_abort_threshold: 6,
            max_messages: 80,
            auto_complete_threshold: 3,
        }
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
    /// Agent needs user input. Content is the question.
    WaitingForInput {
        question: String,
        tool_call_id: String,
    },
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
    let mut completion_detector = CompletionDetector::new(config.auto_complete_threshold);
    let mut parse_retries: usize = 0;

    for step in 1..=config.max_steps {
        // Sliding window: trim messages if over limit
        if config.max_messages > 0 && messages.len() > config.max_messages {
            trim_messages(messages, config.max_messages);
        }
        ctx.iteration = step;
        on_event(LoopEvent::StepStart { step });

        // Lifecycle hook: prepare context
        agent.prepare_context(ctx, messages);

        // Lifecycle hook: prepare tools (filter/reorder)
        let active_tool_names = agent.prepare_tools(ctx, tools);
        let filtered_tools = if active_tool_names.len() == tools.list().len() {
            None // no filtering needed
        } else {
            Some(active_tool_names)
        };

        // Use filtered registry if hooks modified the tool set
        let effective_tools = if let Some(ref names) = filtered_tools {
            &tools.filter(names)
        } else {
            tools
        };

        let decision = match agent.decide(messages, effective_tools).await {
            Ok(d) => {
                parse_retries = 0;
                d
            }
            Err(e) if is_recoverable_error(&e) => {
                parse_retries += 1;
                if parse_retries > MAX_PARSE_RETRIES {
                    return Err(e);
                }
                let err_msg = format!(
                    "Parse error (attempt {}/{}): {}. Please respond with valid JSON matching the schema.",
                    parse_retries, MAX_PARSE_RETRIES, e
                );
                on_event(LoopEvent::Error(AgentError::Llm(SgrError::Schema(err_msg.clone()))));
                messages.push(Message::user(&err_msg));
                continue;
            }
            Err(e) => return Err(e),
        };
        on_event(LoopEvent::Decision(decision.clone()));

        // Auto-completion: detect when agent is done but forgot to call finish_task
        if completion_detector.check(&decision) {
            ctx.state = AgentState::Completed;
            if !decision.situation.is_empty() {
                messages.push(Message::assistant(&decision.situation));
            }
            on_event(LoopEvent::Completed { steps: step });
            return Ok(step);
        }

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

        // Execute tool calls: read-only in parallel, write sequentially
        let mut step_outputs: Vec<String> = Vec::new();
        let mut early_done = false;

        // Partition into read-only (parallel) and write (sequential) tool calls
        let (ro_calls, rw_calls): (Vec<_>, Vec<_>) = decision
            .tool_calls
            .iter()
            .partition(|tc| tools.get(&tc.name).is_some_and(|t| t.is_read_only()));

        // Phase 1: read-only tools in parallel
        if !ro_calls.is_empty() {
            let futs: Vec<_> = ro_calls
                .iter()
                .map(|tc| {
                    let tool = tools.get(&tc.name).unwrap();
                    let args = tc.arguments.clone();
                    let name = tc.name.clone();
                    let id = tc.id.clone();
                    async move { (id, name, tool.execute_readonly(args).await) }
                })
                .collect();

            for (id, name, result) in join_all(futs).await {
                match result {
                    Ok(output) => {
                        on_event(LoopEvent::ToolResult {
                            name: name.clone(),
                            output: output.content.clone(),
                        });
                        step_outputs.push(output.content.clone());
                        agent.after_action(ctx, &name, &output.content);
                        if output.waiting {
                            ctx.state = AgentState::WaitingInput;
                            on_event(LoopEvent::WaitingForInput {
                                question: output.content.clone(),
                                tool_call_id: id.clone(),
                            });
                            messages.push(Message::tool(&id, "[waiting for user input]"));
                            ctx.state = AgentState::Running;
                        } else {
                            messages.push(Message::tool(&id, &output.content));
                        }
                        if output.done {
                            early_done = true;
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Tool error: {}", e);
                        step_outputs.push(err_msg.clone());
                        messages.push(Message::tool(&id, &err_msg));
                        agent.after_action(ctx, &name, &err_msg);
                        on_event(LoopEvent::ToolResult {
                            name,
                            output: err_msg,
                        });
                    }
                }
            }
            if early_done && rw_calls.is_empty() {
                // Only honor early done from read-only tools if no write tools pending
                ctx.state = AgentState::Completed;
                on_event(LoopEvent::Completed { steps: step });
                return Ok(step);
            }
        }

        // Phase 2: write tools sequentially (need &mut ctx)
        for tc in &rw_calls {
            if let Some(tool) = tools.get(&tc.name) {
                match tool.execute(tc.arguments.clone(), ctx).await {
                    Ok(output) => {
                        on_event(LoopEvent::ToolResult {
                            name: tc.name.clone(),
                            output: output.content.clone(),
                        });
                        step_outputs.push(output.content.clone());
                        agent.after_action(ctx, &tc.name, &output.content);
                        if output.waiting {
                            ctx.state = AgentState::WaitingInput;
                            on_event(LoopEvent::WaitingForInput {
                                question: output.content.clone(),
                                tool_call_id: tc.id.clone(),
                            });
                            messages.push(Message::tool(&tc.id, "[waiting for user input]"));
                            ctx.state = AgentState::Running;
                        } else {
                            messages.push(Message::tool(&tc.id, &output.content));
                        }
                        if output.done {
                            ctx.state = AgentState::Completed;
                            on_event(LoopEvent::Completed { steps: step });
                            return Ok(step);
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Tool error: {}", e);
                        step_outputs.push(err_msg.clone());
                        messages.push(Message::tool(&tc.id, &err_msg));
                        agent.after_action(ctx, &tc.name, &err_msg);
                        on_event(LoopEvent::ToolResult {
                            name: tc.name.clone(),
                            output: err_msg,
                        });
                    }
                }
            } else {
                let err_msg = format!("Unknown tool: {}", tc.name);
                step_outputs.push(err_msg.clone());
                messages.push(Message::tool(&tc.id, &err_msg));
                on_event(LoopEvent::ToolResult {
                    name: tc.name.clone(),
                    output: err_msg,
                });
            }
        }

        // Tier 3: output stagnation
        if detector.check_outputs(&step_outputs) {
            ctx.state = AgentState::Failed;
            on_event(LoopEvent::LoopDetected { count: detector.output_repeat_count });
            return Err(AgentError::LoopDetected(detector.output_repeat_count));
        }
    }

    ctx.state = AgentState::Failed;
    Err(AgentError::MaxSteps(config.max_steps))
}

/// Run the agent loop with interactive input support.
///
/// When a tool returns `ToolOutput::waiting`, the loop pauses and calls `on_input`
/// with the question. The returned string is injected as the tool result, then the loop continues.
///
/// This is the interactive version of `run_loop` — use it when the agent may need
/// to ask the user questions (via ClarificationTool or similar).
pub async fn run_loop_interactive<F, Fut>(
    agent: &dyn Agent,
    tools: &ToolRegistry,
    ctx: &mut AgentContext,
    messages: &mut Vec<Message>,
    config: &LoopConfig,
    mut on_event: impl FnMut(LoopEvent),
    mut on_input: F,
) -> Result<usize, AgentError>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = String>,
{
    let mut detector = LoopDetector::new(config.loop_abort_threshold);
    let mut completion_detector = CompletionDetector::new(config.auto_complete_threshold);
    let mut parse_retries: usize = 0;

    for step in 1..=config.max_steps {
        if config.max_messages > 0 && messages.len() > config.max_messages {
            trim_messages(messages, config.max_messages);
        }
        ctx.iteration = step;
        on_event(LoopEvent::StepStart { step });

        agent.prepare_context(ctx, messages);

        let active_tool_names = agent.prepare_tools(ctx, tools);
        let filtered_tools = if active_tool_names.len() == tools.list().len() {
            None
        } else {
            Some(active_tool_names)
        };
        let effective_tools = if let Some(ref names) = filtered_tools {
            &tools.filter(names)
        } else {
            tools
        };

        let decision = match agent.decide(messages, effective_tools).await {
            Ok(d) => {
                parse_retries = 0;
                d
            }
            Err(e) if is_recoverable_error(&e) => {
                parse_retries += 1;
                if parse_retries > MAX_PARSE_RETRIES {
                    return Err(e);
                }
                let err_msg = format!(
                    "Parse error (attempt {}/{}): {}. Please respond with valid JSON matching the schema.",
                    parse_retries, MAX_PARSE_RETRIES, e
                );
                on_event(LoopEvent::Error(AgentError::Llm(SgrError::Schema(err_msg.clone()))));
                messages.push(Message::user(&err_msg));
                continue;
            }
            Err(e) => return Err(e),
        };
        on_event(LoopEvent::Decision(decision.clone()));

        if completion_detector.check(&decision) {
            ctx.state = AgentState::Completed;
            if !decision.situation.is_empty() {
                messages.push(Message::assistant(&decision.situation));
            }
            on_event(LoopEvent::Completed { steps: step });
            return Ok(step);
        }

        if decision.completed || decision.tool_calls.is_empty() {
            ctx.state = AgentState::Completed;
            if !decision.situation.is_empty() {
                messages.push(Message::assistant(&decision.situation));
            }
            on_event(LoopEvent::Completed { steps: step });
            return Ok(step);
        }

        let sig: Vec<String> = decision.tool_calls.iter().map(|tc| tc.name.clone()).collect();
        if detector.check(&sig) {
            ctx.state = AgentState::Failed;
            on_event(LoopEvent::LoopDetected { count: detector.consecutive });
            return Err(AgentError::LoopDetected(detector.consecutive));
        }

        let mut step_outputs: Vec<String> = Vec::new();
        let mut early_done = false;

        // Partition into read-only (parallel) and write (sequential) tool calls
        let (ro_calls, rw_calls): (Vec<_>, Vec<_>) = decision
            .tool_calls
            .iter()
            .partition(|tc| tools.get(&tc.name).is_some_and(|t| t.is_read_only()));

        // Phase 1: read-only tools in parallel
        if !ro_calls.is_empty() {
            let futs: Vec<_> = ro_calls
                .iter()
                .map(|tc| {
                    let tool = tools.get(&tc.name).unwrap();
                    let args = tc.arguments.clone();
                    let name = tc.name.clone();
                    let id = tc.id.clone();
                    async move { (id, name, tool.execute_readonly(args).await) }
                })
                .collect();

            for (id, name, result) in join_all(futs).await {
                match result {
                    Ok(output) => {
                        on_event(LoopEvent::ToolResult {
                            name: name.clone(),
                            output: output.content.clone(),
                        });
                        step_outputs.push(output.content.clone());
                        agent.after_action(ctx, &name, &output.content);
                        if output.waiting {
                            ctx.state = AgentState::WaitingInput;
                            on_event(LoopEvent::WaitingForInput {
                                question: output.content.clone(),
                                tool_call_id: id.clone(),
                            });
                            let response = on_input(output.content).await;
                            ctx.state = AgentState::Running;
                            messages.push(Message::tool(&id, &response));
                        } else {
                            messages.push(Message::tool(&id, &output.content));
                        }
                        if output.done {
                            early_done = true;
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Tool error: {}", e);
                        step_outputs.push(err_msg.clone());
                        messages.push(Message::tool(&id, &err_msg));
                        agent.after_action(ctx, &name, &err_msg);
                        on_event(LoopEvent::ToolResult {
                            name,
                            output: err_msg,
                        });
                    }
                }
            }
            if early_done && rw_calls.is_empty() {
                // Only honor early done from read-only tools if no write tools pending
                ctx.state = AgentState::Completed;
                on_event(LoopEvent::Completed { steps: step });
                return Ok(step);
            }
        }

        // Phase 2: write tools sequentially (need &mut ctx)
        for tc in &rw_calls {
            if let Some(tool) = tools.get(&tc.name) {
                match tool.execute(tc.arguments.clone(), ctx).await {
                    Ok(output) => {
                        on_event(LoopEvent::ToolResult {
                            name: tc.name.clone(),
                            output: output.content.clone(),
                        });
                        step_outputs.push(output.content.clone());
                        agent.after_action(ctx, &tc.name, &output.content);
                        if output.waiting {
                            ctx.state = AgentState::WaitingInput;
                            on_event(LoopEvent::WaitingForInput {
                                question: output.content.clone(),
                                tool_call_id: tc.id.clone(),
                            });
                            let response = on_input(output.content.clone()).await;
                            ctx.state = AgentState::Running;
                            messages.push(Message::tool(&tc.id, &response));
                        } else {
                            messages.push(Message::tool(&tc.id, &output.content));
                        }
                        if output.done {
                            ctx.state = AgentState::Completed;
                            on_event(LoopEvent::Completed { steps: step });
                            return Ok(step);
                        }
                    }
                    Err(e) => {
                        let err_msg = format!("Tool error: {}", e);
                        step_outputs.push(err_msg.clone());
                        messages.push(Message::tool(&tc.id, &err_msg));
                        agent.after_action(ctx, &tc.name, &err_msg);
                        on_event(LoopEvent::ToolResult {
                            name: tc.name.clone(),
                            output: err_msg,
                        });
                    }
                }
            } else {
                let err_msg = format!("Unknown tool: {}", tc.name);
                step_outputs.push(err_msg.clone());
                messages.push(Message::tool(&tc.id, &err_msg));
                on_event(LoopEvent::ToolResult {
                    name: tc.name.clone(),
                    output: err_msg,
                });
            }
        }

        if detector.check_outputs(&step_outputs) {
            ctx.state = AgentState::Failed;
            on_event(LoopEvent::LoopDetected { count: detector.output_repeat_count });
            return Err(AgentError::LoopDetected(detector.output_repeat_count));
        }
    }

    ctx.state = AgentState::Failed;
    Err(AgentError::MaxSteps(config.max_steps))
}

/// 3-tier loop detection:
/// - Tier 1: exact action signature repeats N times consecutively
/// - Tier 2: single tool dominates >90% of all calls
/// - Tier 3: tool output stagnation — same results repeating
struct LoopDetector {
    threshold: usize,
    consecutive: usize,
    last_sig: Vec<String>,
    tool_freq: HashMap<String, usize>,
    total_calls: usize,
    /// Tier 3: hash of last tool outputs to detect stagnation
    last_output_hash: u64,
    output_repeat_count: usize,
}

impl LoopDetector {
    fn new(threshold: usize) -> Self {
        Self {
            threshold,
            consecutive: 0,
            last_sig: vec![],
            tool_freq: HashMap::new(),
            total_calls: 0,
            last_output_hash: 0,
            output_repeat_count: 0,
        }
    }

    /// Check action signature for loop. Returns true if a loop is detected.
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
            for count in self.tool_freq.values() {
                if *count >= self.threshold && *count as f64 / self.total_calls as f64 > 0.9 {
                    return true;
                }
            }
        }

        false
    }

    /// Check tool outputs for stagnation (tier 3). Call after executing tools each step.
    fn check_outputs(&mut self, outputs: &[String]) -> bool {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        outputs.hash(&mut hasher);
        let hash = hasher.finish();

        if hash == self.last_output_hash && self.last_output_hash != 0 {
            self.output_repeat_count += 1;
        } else {
            self.output_repeat_count = 1;
            self.last_output_hash = hash;
        }

        self.output_repeat_count >= self.threshold
    }
}

/// Auto-completion detector — catches when agent is done but doesn't call finish_task.
///
/// Signals completion when:
/// - Agent returns same situation text N times (stuck describing same state)
/// - Situation contains completion keywords ("complete", "finished", "done", "no more")
struct CompletionDetector {
    threshold: usize,
    last_situation: String,
    repeat_count: usize,
}

/// Keywords in situation text that suggest task is complete.
const COMPLETION_KEYWORDS: &[&str] = &[
    "task is complete",
    "task is done",
    "task is finished",
    "all done",
    "successfully completed",
    "nothing more",
    "no further action",
    "no more steps",
];

impl CompletionDetector {
    fn new(threshold: usize) -> Self {
        Self {
            threshold: threshold.max(2),
            last_situation: String::new(),
            repeat_count: 0,
        }
    }

    /// Check if the decision indicates implicit completion.
    fn check(&mut self, decision: &Decision) -> bool {
        // Don't interfere with explicit completion
        if decision.completed || decision.tool_calls.is_empty() {
            return false;
        }

        // Check for completion keywords in situation
        let sit_lower = decision.situation.to_lowercase();
        for keyword in COMPLETION_KEYWORDS {
            if sit_lower.contains(keyword) {
                return true;
            }
        }

        // Check for repeated situation text (agent stuck describing same state)
        if !decision.situation.is_empty() && decision.situation == self.last_situation {
            self.repeat_count += 1;
        } else {
            self.repeat_count = 1;
            self.last_situation = decision.situation.clone();
        }

        self.repeat_count >= self.threshold
    }
}

/// Trim messages to fit within max_messages limit.
/// Keeps: first 2 messages (system + initial user) + last (max - 2) messages.
fn trim_messages(messages: &mut Vec<Message>, max: usize) {
    if messages.len() <= max || max < 4 {
        return;
    }
    let keep_start = 2; // system + user prompt
    // Account for the summary message we'll insert (+1)
    let remove_count = messages.len() - max + 1;
    let removed_range = keep_start..keep_start + remove_count;

    let summary = format!(
        "[{} messages trimmed from context to stay within {} message limit]",
        remove_count, max
    );

    messages.drain(removed_range);
    messages.insert(keep_start, Message::system(&summary));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Agent, AgentError, Decision};
    use crate::agent_tool::{Tool, ToolError, ToolOutput};
    use crate::context::AgentContext;
    use crate::registry::ToolRegistry;
    use crate::types::{Message, SgrError, ToolCall};
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
        let config = LoopConfig {
            max_steps: 50,
            loop_abort_threshold: 3,
            auto_complete_threshold: 100, // disable auto-complete for this test
            ..Default::default()
        };

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
        let config = LoopConfig { max_steps: 5, loop_abort_threshold: 100, ..Default::default() };

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

    #[test]
    fn loop_detector_output_stagnation() {
        let mut d = LoopDetector::new(3);
        let outputs = vec!["same result".to_string()];
        assert!(!d.check_outputs(&outputs));
        assert!(!d.check_outputs(&outputs));
        assert!(d.check_outputs(&outputs)); // 3rd repeat
    }

    #[test]
    fn completion_detector_keyword() {
        let mut cd = CompletionDetector::new(3);
        let d = Decision {
            situation: "The task is complete, all files written.".into(),
            task: vec![],
            tool_calls: vec![ToolCall { id: "1".into(), name: "echo".into(), arguments: serde_json::json!({}) }],
            completed: false,
        };
        assert!(cd.check(&d));
    }

    #[test]
    fn completion_detector_repeated_situation() {
        let mut cd = CompletionDetector::new(3);
        let d = Decision {
            situation: "working on it".into(),
            task: vec![],
            tool_calls: vec![ToolCall { id: "1".into(), name: "echo".into(), arguments: serde_json::json!({}) }],
            completed: false,
        };
        assert!(!cd.check(&d));
        assert!(!cd.check(&d));
        assert!(cd.check(&d)); // 3rd repeat
    }

    #[test]
    fn completion_detector_ignores_explicit_completion() {
        let mut cd = CompletionDetector::new(2);
        let d = Decision {
            situation: "task is complete".into(),
            task: vec![],
            tool_calls: vec![],
            completed: true,
        };
        // Should return false — let normal completion handling take over
        assert!(!cd.check(&d));
    }

    #[test]
    fn trim_messages_basic() {
        let mut msgs: Vec<Message> = (0..10).map(|i| Message::user(&format!("msg {i}"))).collect();
        trim_messages(&mut msgs, 6);
        // first 2 + summary + last 3 = 6
        assert_eq!(msgs.len(), 6);
        assert!(msgs[2].content.contains("trimmed"));
    }

    #[test]
    fn trim_messages_no_op_when_under_limit() {
        let mut msgs = vec![Message::user("a"), Message::user("b")];
        trim_messages(&mut msgs, 10);
        assert_eq!(msgs.len(), 2);
    }

    #[test]
    fn loop_detector_output_stagnation_resets_on_change() {
        let mut d = LoopDetector::new(3);
        let a = vec!["result A".to_string()];
        let b = vec!["result B".to_string()];
        assert!(!d.check_outputs(&a));
        assert!(!d.check_outputs(&a));
        assert!(!d.check_outputs(&b)); // different → resets
        assert!(!d.check_outputs(&a));
    }

    #[tokio::test]
    async fn loop_handles_non_recoverable_llm_error() {
        struct FailingAgent;
        #[async_trait::async_trait]
        impl Agent for FailingAgent {
            async fn decide(&self, _: &[Message], _: &ToolRegistry) -> Result<Decision, AgentError> {
                Err(AgentError::Llm(SgrError::Api {
                    status: 500,
                    body: "internal server error".into(),
                }))
            }
        }

        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();

        let result = run_loop(&FailingAgent, &tools, &mut ctx, &mut messages, &config, |_| {}).await;
        // Non-recoverable: should fail immediately, no retries
        assert!(result.is_err());
        assert_eq!(messages.len(), 1); // no feedback messages added
    }

    #[tokio::test]
    async fn loop_recovers_from_parse_error() {
        // Agent fails with parse error on first call, succeeds on retry
        struct ParseRetryAgent {
            call_count: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl Agent for ParseRetryAgent {
            async fn decide(&self, msgs: &[Message], _: &ToolRegistry) -> Result<Decision, AgentError> {
                let n = self.call_count.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // First call: simulate parse error
                    Err(AgentError::Llm(SgrError::Schema("Missing required field: situation".into())))
                } else {
                    // Second call: should see error feedback in messages
                    let last = msgs.last().unwrap();
                    assert!(last.content.contains("Parse error"), "expected parse error feedback, got: {}", last.content);
                    Ok(Decision {
                        situation: "recovered from parse error".into(),
                        task: vec![],
                        tool_calls: vec![],
                        completed: true,
                    })
                }
            }
        }

        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();
        let agent = ParseRetryAgent { call_count: Arc::new(AtomicUsize::new(0)) };

        let steps = run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |_| {}).await.unwrap();
        assert_eq!(steps, 2); // step 1 failed parse, step 2 succeeded
        assert_eq!(ctx.state, AgentState::Completed);
    }

    #[tokio::test]
    async fn loop_aborts_after_max_parse_retries() {
        struct AlwaysFailParseAgent;
        #[async_trait::async_trait]
        impl Agent for AlwaysFailParseAgent {
            async fn decide(&self, _: &[Message], _: &ToolRegistry) -> Result<Decision, AgentError> {
                Err(AgentError::Llm(SgrError::Schema("bad json".into())))
            }
        }

        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();

        let result = run_loop(&AlwaysFailParseAgent, &tools, &mut ctx, &mut messages, &config, |_| {}).await;
        assert!(result.is_err());
        // Should have added MAX_PARSE_RETRIES feedback messages
        let feedback_count = messages.iter().filter(|m| m.content.contains("Parse error")).count();
        assert_eq!(feedback_count, MAX_PARSE_RETRIES);
    }

    #[tokio::test]
    async fn loop_feeds_tool_errors_back() {
        // Agent calls unknown tool → error fed back → agent completes
        struct ErrorRecoveryAgent {
            call_count: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl Agent for ErrorRecoveryAgent {
            async fn decide(&self, msgs: &[Message], _: &ToolRegistry) -> Result<Decision, AgentError> {
                let n = self.call_count.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // First: call unknown tool
                    Ok(Decision {
                        situation: "trying".into(),
                        task: vec![],
                        tool_calls: vec![ToolCall {
                            id: "1".into(),
                            name: "nonexistent_tool".into(),
                            arguments: serde_json::json!({}),
                        }],
                        completed: false,
                    })
                } else {
                    // Second: should see error in messages, complete
                    let last = msgs.last().unwrap();
                    assert!(last.content.contains("Unknown tool"));
                    Ok(Decision {
                        situation: "recovered".into(),
                        task: vec![],
                        tool_calls: vec![],
                        completed: true,
                    })
                }
            }
        }

        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();
        let agent = ErrorRecoveryAgent { call_count: Arc::new(AtomicUsize::new(0)) };

        let steps = run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |_| {}).await.unwrap();
        assert_eq!(steps, 2);
        assert_eq!(ctx.state, AgentState::Completed);
    }

    #[tokio::test]
    async fn parallel_readonly_tools() {
        struct ReadOnlyTool {
            name: &'static str,
        }

        #[async_trait::async_trait]
        impl Tool for ReadOnlyTool {
            fn name(&self) -> &str {
                self.name
            }
            fn description(&self) -> &str {
                "read-only tool"
            }
            fn is_read_only(&self) -> bool {
                true
            }
            fn parameters_schema(&self) -> Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(
                &self,
                _: Value,
                _: &mut AgentContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::text(format!("{} result", self.name)))
            }
            async fn execute_readonly(&self, _: Value) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::text(format!("{} result", self.name)))
            }
        }

        struct ParallelAgent;
        #[async_trait::async_trait]
        impl Agent for ParallelAgent {
            async fn decide(
                &self,
                msgs: &[Message],
                _: &ToolRegistry,
            ) -> Result<Decision, AgentError> {
                if msgs.len() > 3 {
                    return Ok(Decision {
                        situation: "done".into(),
                        task: vec![],
                        tool_calls: vec![],
                        completed: true,
                    });
                }
                Ok(Decision {
                    situation: "reading".into(),
                    task: vec![],
                    tool_calls: vec![
                        ToolCall {
                            id: "1".into(),
                            name: "reader_a".into(),
                            arguments: serde_json::json!({}),
                        },
                        ToolCall {
                            id: "2".into(),
                            name: "reader_b".into(),
                            arguments: serde_json::json!({}),
                        },
                    ],
                    completed: false,
                })
            }
        }

        let tools = ToolRegistry::new()
            .register(ReadOnlyTool { name: "reader_a" })
            .register(ReadOnlyTool { name: "reader_b" });
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("read stuff")];
        let config = LoopConfig::default();

        let steps = run_loop(
            &ParallelAgent,
            &tools,
            &mut ctx,
            &mut messages,
            &config,
            |_| {},
        )
        .await
        .unwrap();
        assert!(steps > 0);
        assert_eq!(ctx.state, AgentState::Completed);
    }

    #[tokio::test]
    async fn loop_events_are_emitted() {
        let agent = CountingAgent {
            max_calls: 1,
            call_count: Arc::new(AtomicUsize::new(0)),
        };
        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();

        let mut events = Vec::new();
        run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |e| {
            events.push(format!("{:?}", std::mem::discriminant(&e)));
        }).await.unwrap();

        // Should have: StepStart, Decision, ToolResult, StepStart, Decision, Completed
        assert!(events.len() >= 4);
    }
}
