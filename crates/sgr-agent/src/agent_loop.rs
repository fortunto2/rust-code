//! Generic agent loop — drives agent + tools until completion or limit.
//!
//! Includes 3-tier loop detection (exact signature, tool name frequency, output stagnation).
//! Features from Claude Code (HitCC analysis):
//! - Tool result pairing repair — ensures every tool_use has a matching tool_result
//! - Context modifiers — tools can adjust runtime behavior via ToolOutput::modifier
//! - Max output tokens recovery — auto-continuation when response is truncated

use crate::agent::{Agent, AgentError, Decision};
use crate::context::{AgentContext, AgentState};
use crate::registry::ToolRegistry;
use crate::retry::{RetryConfig, delay_for_attempt, is_retryable};
use crate::types::{Message, Role, SgrError};
use futures::future::join_all;
use std::collections::HashMap;

/// Max consecutive parsing errors before aborting the loop.
const MAX_PARSE_RETRIES: usize = 3;

/// Max retries for transient LLM errors (rate limit, timeout, 5xx).
const MAX_TRANSIENT_RETRIES: usize = 3;

/// Max auto-continuation attempts when response is truncated (max_output_tokens).
const MAX_OUTPUT_TOKENS_RECOVERIES: usize = 3;

/// Check if an agent error is recoverable (parsing/empty response).
fn is_recoverable_error(e: &AgentError) -> bool {
    matches!(
        e,
        AgentError::Llm(SgrError::Json(_))
            | AgentError::Llm(SgrError::EmptyResponse)
            | AgentError::Llm(SgrError::Schema(_))
    )
}

/// Wrap `agent.decide_stateful()` with retry for transient LLM errors (rate limit, timeout, 5xx).
/// Parse errors and tool errors are NOT retried here (handled by the caller).
async fn decide_with_retry(
    agent: &dyn Agent,
    messages: &[Message],
    tools: &ToolRegistry,
    previous_response_id: Option<&str>,
) -> Result<(Decision, Option<String>), AgentError> {
    let retry_config = RetryConfig {
        max_retries: MAX_TRANSIENT_RETRIES,
        base_delay_ms: 500,
        max_delay_ms: 30_000,
    };

    for attempt in 0..=retry_config.max_retries {
        match agent
            .decide_stateful(messages, tools, previous_response_id)
            .await
        {
            Ok(d) => return Ok(d),
            Err(AgentError::Llm(sgr_err))
                if is_retryable(&sgr_err) && attempt < retry_config.max_retries =>
            {
                let delay = delay_for_attempt(attempt, &retry_config, &sgr_err);
                tracing::warn!(
                    attempt = attempt + 1,
                    max = retry_config.max_retries,
                    delay_ms = delay.as_millis() as u64,
                    "Retrying agent.decide(): {}",
                    sgr_err
                );
                tokio::time::sleep(delay).await;
                // Loop continues — on last attempt, fall through to return the error
            }
            Err(e) => return Err(e),
        }
    }
    // If we exhausted all retries, do one final attempt and return its result directly
    agent
        .decide_stateful(messages, tools, previous_response_id)
        .await
}

/// Ensure every tool_use in messages has a matching tool_result, and vice versa.
///
/// Repairs the transcript before sending to the API — prevents crashes from:
/// - Tool panics/timeouts leaving orphaned tool_use without tool_result
/// - Duplicate tool_results for the same tool_use_id
/// - Orphaned tool_results without a preceding tool_use
///
/// This is called before each `agent.decide()` call to keep the transcript valid.
pub fn ensure_tool_result_pairing(messages: &mut Vec<Message>) {
    // Collect all tool_use IDs from assistant messages
    let mut expected_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for msg in messages.iter() {
        if msg.role == Role::Assistant {
            for tc in &msg.tool_calls {
                expected_ids.insert(tc.id.clone());
            }
        }
    }

    // Collect all tool_result IDs already present
    let mut seen_result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    // Track indices of duplicate tool_results to remove
    let mut to_remove: Vec<usize> = Vec::new();

    for (i, msg) in messages.iter().enumerate() {
        if msg.role == Role::Tool
            && let Some(ref id) = msg.tool_call_id
        {
            if !seen_result_ids.insert(id.clone()) {
                // Duplicate tool_result — mark for removal
                to_remove.push(i);
            } else if !expected_ids.contains(id) {
                // Orphaned tool_result (no matching tool_use) — mark for removal
                to_remove.push(i);
            }
        }
    }

    // Remove duplicates/orphans in reverse order to preserve indices
    for i in to_remove.into_iter().rev() {
        tracing::debug!(
            tool_call_id = messages[i].tool_call_id.as_deref().unwrap_or("?"),
            "Removing orphaned/duplicate tool_result"
        );
        messages.remove(i);
    }

    // Add synthetic tool_results for missing pairs
    // Walk through messages and insert after each assistant+tool_calls block
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role == Role::Assistant && !messages[i].tool_calls.is_empty() {
            let tool_call_ids: Vec<String> = messages[i]
                .tool_calls
                .iter()
                .map(|tc| tc.id.clone())
                .collect();

            // Check which IDs have results in the subsequent Tool messages
            let mut insert_pos = i + 1;
            let mut found_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
            while insert_pos < messages.len() && messages[insert_pos].role == Role::Tool {
                if let Some(ref id) = messages[insert_pos].tool_call_id {
                    found_ids.insert(id.clone());
                }
                insert_pos += 1;
            }

            // Insert synthetic results for missing IDs
            for id in &tool_call_ids {
                if !found_ids.contains(id) {
                    tracing::debug!(
                        tool_call_id = id.as_str(),
                        "Inserting synthetic tool_result for orphaned tool_use"
                    );
                    messages.insert(
                        insert_pos,
                        Message::tool(id, "[Tool result missing due to internal error]"),
                    );
                    insert_pos += 1;
                }
            }
            i = insert_pos;
        } else {
            i += 1;
        }
    }
}

/// Apply a context modifier from a tool output to the agent context and loop config.
fn apply_context_modifier(
    modifier: &crate::agent_tool::ContextModifier,
    ctx: &mut AgentContext,
    messages: &mut Vec<Message>,
    effective_max_steps: &mut usize,
) {
    if let Some(ref injection) = modifier.system_injection {
        // Use Role::User, not Role::System — mid-conversation system messages
        // are unsupported by Gemini and silently dropped by some providers.
        messages.push(Message::user(format!("[Context update]: {injection}")));
    }
    for (key, value) in &modifier.custom_context {
        ctx.set(key.clone(), value.clone());
    }
    if let Some(delta) = modifier.max_steps_delta {
        if delta > 0 {
            *effective_max_steps = effective_max_steps.saturating_add(delta as usize);
        } else {
            *effective_max_steps =
                effective_max_steps.saturating_sub(delta.unsigned_abs() as usize);
        }
    }
    if let Some(tokens) = modifier.max_tokens_override {
        ctx.set(
            crate::agent_tool::MAX_TOKENS_OVERRIDE_KEY.to_string(),
            serde_json::Value::Number(tokens.into()),
        );
    }
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
    StepStart {
        step: usize,
    },
    Decision(Decision),
    ToolResult {
        name: String,
        output: String,
    },
    Completed {
        steps: usize,
    },
    LoopDetected {
        count: usize,
    },
    Error(AgentError),
    /// Agent needs user input. Content is the question.
    WaitingForInput {
        question: String,
        tool_call_id: String,
    },
    /// Response was truncated, requesting auto-continuation.
    MaxOutputTokensRecovery {
        attempt: usize,
    },
    /// Prompt exceeded model's context limit.
    PromptTooLong {
        message: String,
    },
    /// A tool returned a context modifier that was applied.
    ContextModified {
        tool_name: String,
    },
}

/// Run the agent loop: decide → execute tools → feed results → repeat.
///
/// Returns the number of steps taken.
/// Non-interactive: when a tool returns `ToolOutput::waiting`, emits a
/// `WaitingForInput` event and uses `"[waiting for user input]"` as placeholder.
/// For interactive use (actual user input), use `run_loop_interactive`.
pub async fn run_loop(
    agent: &dyn Agent,
    tools: &ToolRegistry,
    ctx: &mut AgentContext,
    messages: &mut Vec<Message>,
    config: &LoopConfig,
    on_event: impl FnMut(LoopEvent),
) -> Result<usize, AgentError> {
    // Delegate to the unified interactive loop with a passive input handler.
    // When a tool needs input, it gets the placeholder string instead of blocking.
    run_loop_interactive(
        agent,
        tools,
        ctx,
        messages,
        config,
        on_event,
        |_question: String| async { "[waiting for user input]".to_string() },
    )
    .await
}

/// Core agent loop — single implementation for both interactive and non-interactive modes.
///
/// When a tool returns `ToolOutput::waiting`, calls `on_input` with the question.
/// `run_loop` delegates here with a passive handler that returns a placeholder.
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
    let mut response_id: Option<String> = None;
    let mut max_output_tokens_recoveries: usize = 0;
    let mut effective_max_steps = config.max_steps;

    let mut step = 0;
    while {
        step += 1;
        step <= effective_max_steps
    } {
        if config.max_messages > 0 && messages.len() > config.max_messages {
            trim_messages(messages, config.max_messages);
        }

        // Tool result pairing repair
        ensure_tool_result_pairing(messages);

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

        let decision = match decide_with_retry(
            agent,
            messages,
            effective_tools,
            response_id.as_deref(),
        )
        .await
        {
            Ok((d, new_rid)) => {
                parse_retries = 0;
                max_output_tokens_recoveries = 0;
                response_id = new_rid;
                d
            }
            Err(AgentError::Llm(SgrError::MaxOutputTokens { partial_content })) => {
                max_output_tokens_recoveries += 1;
                if max_output_tokens_recoveries > MAX_OUTPUT_TOKENS_RECOVERIES {
                    return Err(AgentError::Llm(SgrError::MaxOutputTokens {
                        partial_content,
                    }));
                }
                if !partial_content.is_empty() {
                    messages.push(Message::assistant(&partial_content));
                }
                messages.push(Message::user(
                    "Your response was cut off. Resume directly from where you stopped. \
                     No apology, no recap — pick up mid-thought.",
                ));
                on_event(LoopEvent::MaxOutputTokensRecovery {
                    attempt: max_output_tokens_recoveries,
                });
                continue;
            }
            Err(AgentError::Llm(SgrError::PromptTooLong(msg))) => {
                on_event(LoopEvent::PromptTooLong {
                    message: msg.clone(),
                });
                return Err(AgentError::Llm(SgrError::PromptTooLong(msg)));
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
                on_event(LoopEvent::Error(AgentError::Llm(SgrError::Schema(
                    err_msg.clone(),
                ))));
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

        let sig: Vec<String> = decision
            .tool_calls
            .iter()
            .map(|tc| tc.name.clone())
            .collect();
        match detector.check(&sig) {
            LoopCheckResult::Abort => {
                ctx.state = AgentState::Failed;
                on_event(LoopEvent::LoopDetected {
                    count: detector.consecutive,
                });
                return Err(AgentError::LoopDetected(detector.consecutive));
            }
            LoopCheckResult::Tier2Warning(dominant_tool) => {
                let hint = format!(
                    "LOOP WARNING: You are repeatedly using '{}' without making progress. \
                     Try a different approach: re-read the file with read_file to see current contents, \
                     use write_file instead of edit_file, or break the problem into smaller steps.",
                    dominant_tool
                );
                messages.push(Message::system(&hint));
            }
            LoopCheckResult::Ok => {}
        }

        // Add assistant message with tool calls (Gemini requires model turn before function responses)
        messages.push(Message::assistant_with_tool_calls(
            &decision.situation,
            decision.tool_calls.clone(),
        ));

        let mut step_outputs: Vec<String> = Vec::new();
        let mut early_done = false;

        // Partition into read-only (parallel) and write (sequential) tool calls
        let (ro_calls, rw_calls): (Vec<_>, Vec<_>) = decision
            .tool_calls
            .iter()
            .partition(|tc| tools.get(&tc.name).is_some_and(|t| t.is_read_only()));

        // Phase 1: read-only tools in parallel (shared read-only context ref)
        if !ro_calls.is_empty() {
            let ctx_snapshot = ctx.clone(); // snapshot for read-only parallel access
            let futs: Vec<_> = ro_calls
                .iter()
                .map(|tc| {
                    let tool = tools.get(&tc.name).unwrap();
                    let args = tc.arguments.clone();
                    let name = tc.name.clone();
                    let id = tc.id.clone();
                    let ctx_ref = &ctx_snapshot;
                    async move { (id, name, tool.execute_readonly(args, ctx_ref).await) }
                })
                .collect();

            let mut pending_modifiers: Vec<(String, crate::agent_tool::ContextModifier)> =
                Vec::new();

            for (id, name, result) in join_all(futs).await {
                match result {
                    Ok(output) => {
                        on_event(LoopEvent::ToolResult {
                            name: name.clone(),
                            output: output.content.clone(),
                        });
                        step_outputs.push(output.content.clone());
                        agent.after_action(ctx, &name, &output.content);
                        if let Some(modifier) = output.modifier.clone()
                            && !modifier.is_empty()
                        {
                            pending_modifiers.push((name.clone(), modifier));
                        }
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

            for (name, modifier) in pending_modifiers {
                apply_context_modifier(&modifier, ctx, messages, &mut effective_max_steps);
                on_event(LoopEvent::ContextModified { tool_name: name });
            }

            if early_done && rw_calls.is_empty() {
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
                        if let Some(ref modifier) = output.modifier
                            && !modifier.is_empty()
                        {
                            apply_context_modifier(
                                modifier,
                                ctx,
                                messages,
                                &mut effective_max_steps,
                            );
                            on_event(LoopEvent::ContextModified {
                                tool_name: tc.name.clone(),
                            });
                        }
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
            on_event(LoopEvent::LoopDetected {
                count: detector.output_repeat_count,
            });
            return Err(AgentError::LoopDetected(detector.output_repeat_count));
        }
    }

    ctx.state = AgentState::Failed;
    Err(AgentError::MaxSteps(effective_max_steps))
}

/// Result of loop detection check.
#[derive(Debug, PartialEq)]
enum LoopCheckResult {
    /// No loop detected.
    Ok,
    /// Tier 2 warning: a single tool category dominates. Contains the dominant tool name.
    /// Agent gets one more chance with a hint injected.
    Tier2Warning(String),
    /// Hard loop detected (tier 1 exact repeat, or tier 2 after warning).
    Abort,
}

/// 3-tier loop detection:
/// - Tier 1: exact action signature repeats N times consecutively
/// - Tier 2: single tool dominates >90% of all calls (warns first, aborts on second trigger)
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
    /// Whether tier 2 warning has already been issued (next trigger aborts).
    tier2_warned: bool,
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
            tier2_warned: false,
        }
    }

    /// Check action signature for loop.
    /// Returns `Abort` for tier 1 (exact repeat) or tier 2 after warning.
    /// Returns `Tier2Warning` on first tier 2 trigger (dominant tool detected).
    fn check(&mut self, sig: &[String]) -> LoopCheckResult {
        self.total_calls += 1;

        // Tier 1: exact signature match
        if sig == self.last_sig {
            self.consecutive += 1;
        } else {
            self.consecutive = 1;
            self.last_sig = sig.to_vec();
        }
        if self.consecutive >= self.threshold {
            return LoopCheckResult::Abort;
        }

        // Tier 2: tool name frequency (single tool dominates)
        for name in sig {
            *self.tool_freq.entry(name.clone()).or_insert(0) += 1;
        }
        if self.total_calls >= self.threshold {
            for (name, count) in &self.tool_freq {
                if *count >= self.threshold && *count as f64 / self.total_calls as f64 > 0.9 {
                    if self.tier2_warned {
                        return LoopCheckResult::Abort;
                    }
                    self.tier2_warned = true;
                    return LoopCheckResult::Tier2Warning(name.clone());
                }
            }
        }

        LoopCheckResult::Ok
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
    let remove_count = messages.len() - max + 1;
    let mut trim_end = keep_start + remove_count;

    // Don't break functionCall → functionResponse pairs.
    // Gemini requires: model turn (functionCall) → user turn (functionResponse).
    // If trim_end lands in the middle of such a pair, extend to skip the whole group.
    //
    // Case 1: trim_end points at Tool messages — extend past them (they'd be orphaned).
    while trim_end < messages.len() && messages[trim_end].role == Role::Tool {
        trim_end += 1;
    }
    // Case 2: the first kept message is a Tool — it lost its preceding Assistant.
    // (Already handled by Case 1, but double-check.)
    //
    // Case 3: the last removed message is an Assistant with tool_calls —
    // the following Tool messages (now first in kept region) would be orphaned.
    // Extend trim_end to also remove those Tool messages.
    if trim_end > keep_start && trim_end < messages.len() {
        let last_removed = trim_end - 1;
        if messages[last_removed].role == Role::Assistant
            && !messages[last_removed].tool_calls.is_empty()
        {
            // The assistant had tool_calls but we're keeping it... actually we're removing it.
            // So remove all following Tool messages too.
            while trim_end < messages.len() && messages[trim_end].role == Role::Tool {
                trim_end += 1;
            }
        }
    }

    let removed_range = keep_start..trim_end;

    let summary = format!(
        "[{} messages trimmed from context to stay within {} message limit]",
        trim_end - keep_start,
        max
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "echo"
        }
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

        let steps = run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |_| {})
            .await
            .unwrap();
        assert_eq!(steps, 4); // 3 tool calls + 1 completion
        assert_eq!(ctx.state, AgentState::Completed);
    }

    #[tokio::test]
    async fn loop_detects_repetition() {
        // Agent always returns same tool call → loop detection
        struct LoopingAgent;
        #[async_trait::async_trait]
        impl Agent for LoopingAgent {
            async fn decide(
                &self,
                _: &[Message],
                _: &ToolRegistry,
            ) -> Result<Decision, AgentError> {
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

        let result = run_loop(
            &LoopingAgent,
            &tools,
            &mut ctx,
            &mut messages,
            &config,
            |_| {},
        )
        .await;
        assert!(matches!(result, Err(AgentError::LoopDetected(3))));
        assert_eq!(ctx.state, AgentState::Failed);
    }

    #[tokio::test]
    async fn loop_max_steps() {
        // Agent never completes
        struct NeverDoneAgent;
        #[async_trait::async_trait]
        impl Agent for NeverDoneAgent {
            async fn decide(
                &self,
                _: &[Message],
                _: &ToolRegistry,
            ) -> Result<Decision, AgentError> {
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
        let config = LoopConfig {
            max_steps: 5,
            loop_abort_threshold: 100,
            ..Default::default()
        };

        let result = run_loop(
            &NeverDoneAgent,
            &tools,
            &mut ctx,
            &mut messages,
            &config,
            |_| {},
        )
        .await;
        assert!(matches!(result, Err(AgentError::MaxSteps(5))));
    }

    #[test]
    fn loop_detector_exact_sig() {
        let mut d = LoopDetector::new(3);
        let sig = vec!["bash".to_string()];
        assert_eq!(d.check(&sig), LoopCheckResult::Ok);
        assert_eq!(d.check(&sig), LoopCheckResult::Ok);
        assert_eq!(d.check(&sig), LoopCheckResult::Abort); // 3rd consecutive
    }

    #[test]
    fn loop_detector_different_sigs_reset() {
        let mut d = LoopDetector::new(3);
        assert_eq!(d.check(&["bash".into()]), LoopCheckResult::Ok);
        assert_eq!(d.check(&["bash".into()]), LoopCheckResult::Ok);
        assert_eq!(d.check(&["read".into()]), LoopCheckResult::Ok); // different → resets
        assert_eq!(d.check(&["bash".into()]), LoopCheckResult::Ok);
    }

    #[test]
    fn loop_detector_tier2_warning_then_abort() {
        // Tier 2 requires: count >= threshold AND count/total > 0.9
        // Use threshold=3. To avoid tier 1 (exact consecutive), alternate sigs.
        let mut d = LoopDetector::new(3);
        // Calls 1-2: build up frequency, total_calls < threshold so tier 2 not checked
        assert_eq!(d.check(&["edit_file".into()]), LoopCheckResult::Ok); // total=1, edit=1, cons=1
        assert_eq!(d.check(&["edit_file".into()]), LoopCheckResult::Ok); // total=2, edit=2, cons=2
        // Call 3: break consecutive (different sig) but edit_file still in sig
        // total=3, edit=3, cons=1 → tier 2: 3/3=1.0 > 0.9 → first warning
        assert_eq!(
            d.check(&["edit_file".into(), "read_file".into()]),
            LoopCheckResult::Tier2Warning("edit_file".into())
        );
        // Call 4: tier 2 already warned → abort
        assert_eq!(d.check(&["edit_file".into()]), LoopCheckResult::Abort);
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
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({}),
            }],
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
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "echo".into(),
                arguments: serde_json::json!({}),
            }],
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
        let mut msgs: Vec<Message> = (0..10).map(|i| Message::user(format!("msg {i}"))).collect();
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
    fn trim_messages_preserves_assistant_tool_call_pair() {
        use crate::types::Role;
        // system, user, assistant(tool_calls), tool, tool, user, assistant
        let mut msgs = vec![
            Message::system("sys"),
            Message::user("prompt"),
            Message::assistant_with_tool_calls(
                "calling",
                vec![
                    ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({}),
                    },
                    ToolCall {
                        id: "c2".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({}),
                    },
                ],
            ),
            Message::tool("c1", "result1"),
            Message::tool("c2", "result2"),
            Message::user("next"),
            Message::assistant("done"),
        ];
        // Trim to 5 — should remove assistant+tools as a group, not split them
        trim_messages(&mut msgs, 5);
        // Verify no orphaned Tool messages remain
        for (i, msg) in msgs.iter().enumerate() {
            if msg.role == Role::Tool {
                // The previous message should be an Assistant with tool_calls
                assert!(i > 0, "Tool message at start");
                assert!(
                    msgs[i - 1].role == Role::Assistant && !msgs[i - 1].tool_calls.is_empty()
                        || msgs[i - 1].role == Role::Tool,
                    "Orphaned Tool at position {i}"
                );
            }
        }
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
            async fn decide(
                &self,
                _: &[Message],
                _: &ToolRegistry,
            ) -> Result<Decision, AgentError> {
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

        let result = run_loop(
            &FailingAgent,
            &tools,
            &mut ctx,
            &mut messages,
            &config,
            |_| {},
        )
        .await;
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
            async fn decide(
                &self,
                msgs: &[Message],
                _: &ToolRegistry,
            ) -> Result<Decision, AgentError> {
                let n = self.call_count.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // First call: simulate parse error
                    Err(AgentError::Llm(SgrError::Schema(
                        "Missing required field: situation".into(),
                    )))
                } else {
                    // Second call: should see error feedback in messages
                    let last = msgs.last().unwrap();
                    assert!(
                        last.content.contains("Parse error"),
                        "expected parse error feedback, got: {}",
                        last.content
                    );
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
        let agent = ParseRetryAgent {
            call_count: Arc::new(AtomicUsize::new(0)),
        };

        let steps = run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |_| {})
            .await
            .unwrap();
        assert_eq!(steps, 2); // step 1 failed parse, step 2 succeeded
        assert_eq!(ctx.state, AgentState::Completed);
    }

    #[tokio::test]
    async fn loop_aborts_after_max_parse_retries() {
        struct AlwaysFailParseAgent;
        #[async_trait::async_trait]
        impl Agent for AlwaysFailParseAgent {
            async fn decide(
                &self,
                _: &[Message],
                _: &ToolRegistry,
            ) -> Result<Decision, AgentError> {
                Err(AgentError::Llm(SgrError::Schema("bad json".into())))
            }
        }

        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();

        let result = run_loop(
            &AlwaysFailParseAgent,
            &tools,
            &mut ctx,
            &mut messages,
            &config,
            |_| {},
        )
        .await;
        assert!(result.is_err());
        // Should have added MAX_PARSE_RETRIES feedback messages
        let feedback_count = messages
            .iter()
            .filter(|m| m.content.contains("Parse error"))
            .count();
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
            async fn decide(
                &self,
                msgs: &[Message],
                _: &ToolRegistry,
            ) -> Result<Decision, AgentError> {
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
        let agent = ErrorRecoveryAgent {
            call_count: Arc::new(AtomicUsize::new(0)),
        };

        let steps = run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |_| {})
            .await
            .unwrap();
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
            async fn execute_readonly(
                &self,
                _: Value,
                _ctx: &crate::context::AgentContext,
            ) -> Result<ToolOutput, ToolError> {
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
        })
        .await
        .unwrap();

        // Should have: StepStart, Decision, ToolResult, StepStart, Decision, Completed
        assert!(events.len() >= 4);
    }

    #[tokio::test]
    async fn tool_output_done_stops_loop() {
        // A tool that returns ToolOutput::done() should stop the loop immediately.
        struct DoneTool;
        #[async_trait::async_trait]
        impl Tool for DoneTool {
            fn name(&self) -> &str {
                "done_tool"
            }
            fn description(&self) -> &str {
                "returns done"
            }
            fn parameters_schema(&self) -> Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(
                &self,
                _: Value,
                _: &mut AgentContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::done("final answer"))
            }
        }

        struct OneShotAgent;
        #[async_trait::async_trait]
        impl Agent for OneShotAgent {
            async fn decide(
                &self,
                _: &[Message],
                _: &ToolRegistry,
            ) -> Result<Decision, AgentError> {
                Ok(Decision {
                    situation: "calling done tool".into(),
                    task: vec![],
                    tool_calls: vec![ToolCall {
                        id: "1".into(),
                        name: "done_tool".into(),
                        arguments: serde_json::json!({}),
                    }],
                    completed: false,
                })
            }
        }

        let tools = ToolRegistry::new().register(DoneTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();

        let steps = run_loop(
            &OneShotAgent,
            &tools,
            &mut ctx,
            &mut messages,
            &config,
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            steps, 1,
            "Loop should stop on first step when tool returns done"
        );
        assert_eq!(ctx.state, AgentState::Completed);
    }

    #[tokio::test]
    async fn tool_messages_formatted_correctly() {
        // Verify that assistant messages with tool_calls are preserved in the message list,
        // followed by tool result messages.
        let agent = CountingAgent {
            max_calls: 1,
            call_count: Arc::new(AtomicUsize::new(0)),
        };
        let tools = ToolRegistry::new().register(EchoTool);
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let config = LoopConfig::default();

        run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |_| {})
            .await
            .unwrap();

        // After 1 tool call + completion, messages should be:
        // [user("go"), assistant_with_tool_calls("step 0", [echo]), tool("echoed"), assistant("done")]
        assert!(messages.len() >= 4);

        // Find the assistant message with tool calls
        let assistant_tc = messages
            .iter()
            .find(|m| m.role == crate::types::Role::Assistant && !m.tool_calls.is_empty());
        assert!(
            assistant_tc.is_some(),
            "Should have an assistant message with tool_calls"
        );
        let atc = assistant_tc.unwrap();
        assert_eq!(atc.tool_calls[0].name, "echo");
        assert_eq!(atc.tool_calls[0].id, "call_0");

        // The next message should be a Tool result
        let tc_idx = messages
            .iter()
            .position(|m| m.role == crate::types::Role::Assistant && !m.tool_calls.is_empty())
            .unwrap();
        let tool_msg = &messages[tc_idx + 1];
        assert_eq!(tool_msg.role, crate::types::Role::Tool);
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_0"));
        assert_eq!(tool_msg.content, "echoed");
    }

    // --- Tool result pairing repair tests ---

    #[test]
    fn pairing_adds_missing_tool_result() {
        let mut msgs = vec![
            Message::user("go"),
            Message::assistant_with_tool_calls(
                "calling",
                vec![
                    ToolCall {
                        id: "c1".into(),
                        name: "bash".into(),
                        arguments: serde_json::json!({}),
                    },
                    ToolCall {
                        id: "c2".into(),
                        name: "read".into(),
                        arguments: serde_json::json!({}),
                    },
                ],
            ),
            // Only c1 has a result — c2 is missing
            Message::tool("c1", "ok"),
        ];
        ensure_tool_result_pairing(&mut msgs);

        // c2 should now have a synthetic result
        let c2_result = msgs
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("c2"));
        assert!(c2_result.is_some(), "Should have synthetic result for c2");
        assert!(c2_result.unwrap().content.contains("missing"));
    }

    #[test]
    fn pairing_removes_duplicate_tool_result() {
        let mut msgs = vec![
            Message::user("go"),
            Message::assistant_with_tool_calls(
                "calling",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({}),
                }],
            ),
            Message::tool("c1", "first"),
            Message::tool("c1", "duplicate"), // duplicate
        ];
        ensure_tool_result_pairing(&mut msgs);

        let c1_count = msgs
            .iter()
            .filter(|m| m.tool_call_id.as_deref() == Some("c1"))
            .count();
        assert_eq!(c1_count, 1, "Should remove duplicate tool_result");
    }

    #[test]
    fn pairing_removes_orphaned_tool_result() {
        let mut msgs = vec![
            Message::user("go"),
            Message::tool("orphan_id", "orphaned result"), // no matching tool_use
            Message::assistant("done"),
        ];
        ensure_tool_result_pairing(&mut msgs);

        let orphan = msgs
            .iter()
            .find(|m| m.tool_call_id.as_deref() == Some("orphan_id"));
        assert!(orphan.is_none(), "Should remove orphaned tool_result");
    }

    #[test]
    fn pairing_noop_for_valid_transcript() {
        let mut msgs = vec![
            Message::user("go"),
            Message::assistant_with_tool_calls(
                "calling",
                vec![ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: serde_json::json!({}),
                }],
            ),
            Message::tool("c1", "result"),
            Message::assistant("done"),
        ];
        let len_before = msgs.len();
        ensure_tool_result_pairing(&mut msgs);
        assert_eq!(msgs.len(), len_before, "Valid transcript should not change");
    }

    // --- Context modifier tests ---

    #[test]
    fn context_modifier_system_injection() {
        use crate::agent_tool::ContextModifier;

        let modifier = ContextModifier::system("Extra instructions for next step");
        let mut ctx = AgentContext::new();
        let mut messages = vec![Message::user("go")];
        let mut max_steps = 50;

        apply_context_modifier(&modifier, &mut ctx, &mut messages, &mut max_steps);

        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].role, Role::User); // User, not System — Gemini compat
        assert!(messages[1].content.contains("Extra instructions"));
    }

    #[test]
    fn context_modifier_extra_steps() {
        use crate::agent_tool::ContextModifier;

        let mut ctx = AgentContext::new();
        let mut messages = vec![];
        let mut max_steps = 50;

        let modifier = ContextModifier::extra_steps(20);
        apply_context_modifier(&modifier, &mut ctx, &mut messages, &mut max_steps);
        assert_eq!(max_steps, 70);

        let modifier = ContextModifier::extra_steps(-10);
        apply_context_modifier(&modifier, &mut ctx, &mut messages, &mut max_steps);
        assert_eq!(max_steps, 60);
    }

    #[test]
    fn context_modifier_custom_context() {
        use crate::agent_tool::ContextModifier;

        let modifier = ContextModifier::custom("my_key", serde_json::json!("my_value"));
        let mut ctx = AgentContext::new();
        let mut messages = vec![];
        let mut max_steps = 50;

        apply_context_modifier(&modifier, &mut ctx, &mut messages, &mut max_steps);

        assert_eq!(ctx.get("my_key").unwrap(), "my_value");
    }

    #[test]
    fn context_modifier_is_empty() {
        use crate::agent_tool::ContextModifier;

        assert!(ContextModifier::default().is_empty());
        assert!(!ContextModifier::system("hi").is_empty());
        assert!(!ContextModifier::max_tokens(100).is_empty());
        assert!(!ContextModifier::extra_steps(5).is_empty());
        assert!(!ContextModifier::custom("k", serde_json::json!("v")).is_empty());
    }

    #[test]
    fn context_modifier_max_tokens_stored_in_context() {
        use crate::agent_tool::ContextModifier;

        let modifier = ContextModifier::max_tokens(4096);
        let mut ctx = AgentContext::new();
        let mut messages = vec![];
        let mut max_steps = 50;

        apply_context_modifier(&modifier, &mut ctx, &mut messages, &mut max_steps);

        assert_eq!(ctx.max_tokens_override(), Some(4096));
    }
}
