use crate::backend::{self, LlmProvider, SgrAction, SgrNextStep};
use crate::rc_state::RcState;
use crate::tools::{
    build_skills_context, cost, create_checkpoint, is_mutating_action, mcp::McpManager,
};
use anyhow::Result;
use sgr_agent::registry::ToolRegistry;
use sgr_agent::swarm::SwarmManager;
use sgr_agent::{
    ActionKind, ActionResult, AgentMessage, HintContext, Intent, LoopDetector, MessageRole,
    Session, SgrAgent, SgrAgentStream, StepDecision, collect_hints,
};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

/// Action type -- the 27-variant tool enum (kept as wire format for LLM parsing).
pub type Action = SgrAction;

// Implement sgr-agent traits for the Message type

#[derive(Clone, PartialEq)]
pub struct Role(String);

impl MessageRole for Role {
    fn system() -> Self {
        Self("system".into())
    }
    fn user() -> Self {
        Self("user".into())
    }
    fn assistant() -> Self {
        Self("assistant".into())
    }
    fn tool() -> Self {
        Self("tool".into())
    }
    fn as_str(&self) -> &str {
        &self.0
    }
    fn parse_role(s: &str) -> Option<Self> {
        match s {
            "system" | "user" | "assistant" | "tool" => Some(Self(s.into())),
            _ => None,
        }
    }
}

/// Chat message for the session.
#[derive(Clone)]
pub struct Msg {
    pub role: String,
    pub content: String,
    /// Inline images (base64 + mime_type) for multimodal input.
    pub images: Vec<sgr_agent::ImagePart>,
    /// Tool call ID for Responses API stateful chaining (function_call_output).
    pub call_id: Option<String>,
}

impl AgentMessage for Msg {
    type Role = Role;
    fn new(role: Role, content: String) -> Self {
        Self {
            role: role.0,
            content,
            images: vec![],
            call_id: None,
        }
    }
    fn with_call_id(mut self, call_id: String) -> Self {
        self.call_id = Some(call_id);
        self
    }
    fn role(&self) -> &Role {
        // Safety: Role is repr(String), same layout
        unsafe { &*(&self.role as *const String as *const Role) }
    }
    fn content(&self) -> &str {
        &self.content
    }
}

pub struct Agent {
    session: Session<Msg>,
    mcp: Arc<Option<McpManager>>,
    step_count: usize,
    last_input_chars: usize,
    /// LLM provider (via LlmConfig -- auto-detects from model name).
    provider: Option<LlmProvider>,
    /// Current user intent for action filtering.
    pub intent: Intent,
    /// Pluggable hint sources.
    hint_sources: Vec<Box<dyn sgr_agent::HintSource>>,
    /// Shared mutable state for tools (cwd, read_cache, edit_failures).
    state: RcState,
    /// Tool registry -- dispatches SgrAction to Tool impls.
    tool_registry: ToolRegistry,
    /// Multi-agent swarm manager for sub-agents.
    swarm: Arc<TokioMutex<SwarmManager>>,
    /// Delegate manager for external CLI agents (claude/gemini/codex/opencode/rust-code).
    delegate_mgr: Arc<TokioMutex<crate::tools::delegate::DelegateManager>>,
    /// OpenAPI registry for the `api` tool.
    api_registry: Arc<TokioMutex<sgr_agent::openapi::ApiRegistry>>,
    /// Last Responses API response_id for stateful chaining (prompt caching).
    last_response_id: std::sync::Mutex<Option<String>>,
}

const AGENT_HOME: &str = ".rust-code";
const MAX_HISTORY: usize = 200;

/// System prompt for SGR backend (replaces BAML's built-in prompt template).
/// Covers: tools, STAR methodology, JSON-only output rule, finish discipline.
const SGR_SYSTEM_PROMPT: &str = r#"You are rust-code, an expert AI coding agent in a Terminal UI.

## Output Format — CRITICAL
You MUST respond with ONLY valid JSON. No markdown. No prose. No code blocks. No explanation. Just raw JSON.
Every response must be: {"situation": "...", "task": ["..."], "actions": [{...}]}

## Tools (use via "tool_name" field in actions array)
- read_file: {tool_name, path, offset?, limit?} — read file contents
- write_file: {tool_name, path, content} — create/overwrite file
- edit_file: {tool_name, path, old_string, new_string} — edit existing file (simple single replacement)
- apply_patch: {tool_name, patch} — apply a patch to one or more files (PREFERRED for edits)
- bash: {tool_name, command, description?, timeout?} — run shell command
- bash_bg: {tool_name, name, command} — run in tmux background
- search_code: {tool_name, query} — ripgrep search
- git_status: {tool_name} — show git status
- git_diff: {tool_name, path?, cached?} — show diff
- git_add: {tool_name, paths} — stage files
- git_commit: {tool_name, message} — commit
- open_editor: {tool_name, path, line?} — open in editor
- ask_user: {tool_name, question} — ask user a question
- finish: {tool_name, summary} — MUST use to complete task, put full answer in summary
- mcp_call: {tool_name, server, tool, arguments?} — call MCP tool
- memory: {tool_name, operation, section, content?} — save/recall memory
- project_map: {tool_name, path?} — scan project structure
- dependencies: {tool_name, path?} — parse dependency files
- task: {tool_name, action, id?, title?, status?, body?} — manage tasks
- spawn_agent: {tool_name, role, task, max_steps?} — spawn sub-agent (explorer/worker/reviewer)
- wait_agents: {tool_name, agent_ids?, timeout_secs?} — wait for sub-agents to complete
- agent_status: {tool_name, agent_id?} — check sub-agent status
- cancel_agent: {tool_name, agent_id} — cancel a sub-agent ("all" for all)
- api: {tool_name, action, api_name?, query?, endpoint?, params?, body?} — REST API tool. Actions: "load" (api_name), "search" (api_name + query), "call" (api_name + endpoint + params="key=val,key2=val2"), "list". Use "api list" to see all available APIs with descriptions. For web search/research, try loading "searxng" API and calling its search endpoint.
- delegate_task: {tool_name, agent, task?, task_path?, cwd?} — delegate to CLI agent (claude/gemini/codex/opencode/rust-code). Use task for free-text, or task_path for .tasks/ file (agent reads it, executes, updates status). Runs in tmux.
- delegate_status: {tool_name, id?} — check delegate status (omit id for all)
- delegate_result: {tool_name, id} — get output from completed delegate

## Self-Update
If you patch your own source code and need to test the fix:
1. Apply patch + run tests to verify
2. `git_add` + `git_commit` — ALWAYS commit before restart
3. `bash: cargo build --release -p rust-code` — build new binary
4. `finish: "RESTART_AGENT — rebuilt with fix for X"` — process auto-restarts with --resume
The session continues in the new binary. ALWAYS commit changes before restart.

## STAR Methodology
- **S (situation)**: Assess current state. What phase? What's done? What's blocking?
- **T (task)**: List 1-5 remaining steps. First item = what you do NOW.
- **A (actions)**: Execute the first task step. Use multiple actions for independent ops.
- **R (result)**: Use finish tool when ALL steps done. Put full answer in summary.

## apply_patch Format
Use apply_patch for file edits (PREFERRED over edit_file). Format:
*** Begin Patch
*** Update File: path/to/file.ts
@@ context_line (class/function name to narrow scope)
 context line (unchanged, prefix with space)
-old line to remove
+new line to add
 context line
*** End Patch

Operations: "*** Add File: path" (new file, +lines), "*** Delete File: path", "*** Update File: path".
Use @@ markers when 3 lines of context aren't enough to uniquely locate the change.
Show 3 lines of context before and after each change. File paths must be relative.

## Rules
- NEVER respond with prose or markdown — ONLY JSON.
- When answering questions or reporting findings, use finish tool with the answer in summary.
- Act directly — don't waste steps on setup.
- Use multiple actions for independent operations (e.g. read 3 files at once).
- Read files before editing. Verify changes with git_status or tests.
- PREFER apply_patch over edit_file for editing files — it handles multiple changes at once.
- Use read_file instead of bash:cat — read_file has caching and better error handling.

## Git Commit — Pre-commit Hook
git_commit runs the project's pre-commit hook (tests, lint, format check).
If commit FAILS, you get the full error output. Fix the issue and retry:
- Format error → run the formatter (`make fmt`, `cargo fmt`, `prettier --write`, etc.)
- Test failure → fix the failing test
- Lint error → fix the warning
- Then `git_add` the fixes and `git_commit` again
Check Makefile for available commands (e.g. `make check`, `make fmt`, `make lint`).

## Anti-Loop Rules — CRITICAL
- NEVER re-read a file you already read. The content is in your conversation history — use it.
- NEVER re-read after apply_patch or write_file — if it succeeded, the file is correct.
- NEVER run bash:cat on a file you already read with read_file (or vice versa).
- If a command returns empty/error, that IS the answer. Do NOT retry with different flags.
- Every step must make FORWARD PROGRESS. If you catch yourself reading the same file, STOP and act.
"#;

impl Agent {
    pub fn new() -> Self {
        let mut session =
            Session::new(AGENT_HOME, MAX_HISTORY).expect("failed to create session directory");

        // Load layered context: agent home (SOUL, IDENTITY, etc.) + project (AGENTS.md/CLAUDE.md + rules)
        let mut ctx = sgr_agent::MemoryContext::load(AGENT_HOME);
        ctx.merge(sgr_agent::MemoryContext::load_project(Path::new(".")));
        if let Some(msg) = ctx.to_system_message() {
            session.push(Role::system(), msg);
        }

        // Inject installed skills context
        if let Some(skills_ctx) = build_skills_context() {
            session.push(Role::system(), skills_ctx);
        }

        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let state = RcState::new(cwd);
        let swarm = Arc::new(TokioMutex::new(SwarmManager::new()));
        let delegate_mgr = Arc::new(TokioMutex::new(
            crate::tools::delegate::DelegateManager::new(),
        ));
        let api_registry = Arc::new(TokioMutex::new(sgr_agent::openapi::ApiRegistry::new()));
        let mcp: Arc<Option<McpManager>> = Arc::new(None);

        // Build tool registry -- each tool holds shared state references
        let tool_registry = Self::build_tool_registry(
            state.clone(),
            swarm.clone(),
            delegate_mgr.clone(),
            api_registry.clone(),
            mcp.clone(),
            None, // provider not yet set
        );

        Self {
            session,
            mcp,
            step_count: 0,
            last_input_chars: 0,
            provider: None,
            intent: Intent::Auto,
            hint_sources: sgr_agent::default_sources_with_tasks(Path::new(".")),
            state,
            tool_registry,
            swarm,
            delegate_mgr,
            api_registry,
            last_response_id: std::sync::Mutex::new(None),
        }
    }

    /// Build the ToolRegistry with all 27 tools.
    fn build_tool_registry(
        state: RcState,
        swarm: Arc<TokioMutex<SwarmManager>>,
        delegate_mgr: Arc<TokioMutex<crate::tools::delegate::DelegateManager>>,
        api_registry: Arc<TokioMutex<sgr_agent::openapi::ApiRegistry>>,
        mcp: Arc<Option<McpManager>>,
        provider: Option<LlmProvider>,
    ) -> ToolRegistry {
        use crate::tools::*;
        ToolRegistry::new()
            .register(read_file_tool::ReadFileTool {
                state: state.clone(),
            })
            .register(write_file_tool::WriteFileTool {
                state: state.clone(),
            })
            .register(edit_file_tool::EditFileTool {
                state: state.clone(),
            })
            .register(apply_patch_tool::ApplyPatchTool {
                state: state.clone(),
            })
            .register(bash_tool::BashTool {
                state: state.clone(),
            })
            .register(bash_tool::BashBgTool)
            .register(search_tool::SearchCodeTool {
                state: state.clone(),
            })
            .register(git_tool::GitStatusTool)
            .register(git_tool::GitDiffTool)
            .register(git_tool::GitAddTool)
            .register(git_tool::GitCommitTool {
                state: state.clone(),
            })
            .register(editor_tool::OpenEditorTool)
            .register(finish_tool::AskUserTool)
            .register(finish_tool::FinishTool)
            .register(mcp_tool::McpCallTool { mcp })
            .register(memory_tool::MemoryTool)
            .register(project_tools::ProjectMapTool)
            .register(project_tools::DependenciesTool)
            .register(task_tool::TaskTool)
            .register(swarm_tools::SpawnAgentTool {
                swarm: swarm.clone(),
                provider: Arc::new(provider),
            })
            .register(swarm_tools::WaitAgentsTool {
                swarm: swarm.clone(),
            })
            .register(swarm_tools::AgentStatusTool {
                swarm: swarm.clone(),
            })
            .register(swarm_tools::CancelAgentTool { swarm })
            .register(api_tool::ApiTool {
                registry: api_registry,
            })
            .register(delegate_tools::DelegateTaskTool {
                state: state.clone(),
                delegate_mgr: delegate_mgr.clone(),
            })
            .register(delegate_tools::DelegateStatusTool {
                delegate_mgr: delegate_mgr.clone(),
            })
            .register(delegate_tools::DelegateResultTool { delegate_mgr })
    }

    /// Set working directory for tool execution.
    pub fn set_cwd(&self, path: std::path::PathBuf) {
        *self.state.cwd.lock().unwrap() = path;
    }

    /// Create a new LoopDetector (used by callers in app.rs/main.rs).
    pub fn new_loop_detector() -> LoopDetector {
        LoopDetector::new(10)
    }

    /// Initialize MCP servers from .mcp.json configs.
    pub async fn init_mcp(&mut self) -> Result<()> {
        let config = McpManager::load_configs();
        if config.mcp_servers.is_empty() {
            return Ok(());
        }

        tracing::info!("Starting {} MCP server(s)...", config.mcp_servers.len());
        let manager = McpManager::start_all(&config).await?;

        if let Some(mcp_ctx) = manager.build_context() {
            self.session.push(Role::system(), mcp_ctx);
        }

        tracing::info!(
            "MCP ready: {} servers, {} tools",
            manager.server_count(),
            manager.tool_count()
        );
        self.mcp = Arc::new(Some(manager));
        // Rebuild tool registry with updated MCP reference
        self.tool_registry = Self::build_tool_registry(
            self.state.clone(),
            self.swarm.clone(),
            self.delegate_mgr.clone(),
            self.api_registry.clone(),
            self.mcp.clone(),
            self.provider.clone(),
        );
        Ok(())
    }

    pub fn mcp(&self) -> Option<&McpManager> {
        self.mcp.as_ref().as_ref()
    }

    pub fn load_last_session(&mut self) -> Result<()> {
        if let Some(resumed) = Session::resume_last(AGENT_HOME, MAX_HISTORY) {
            self.session = resumed;
        }
        Ok(())
    }

    pub fn load_session_file(&mut self, path: &Path) -> Result<()> {
        self.session = Session::resume(path, AGENT_HOME, MAX_HISTORY);
        Ok(())
    }

    pub fn history(&self) -> Vec<&Msg> {
        self.session.messages().iter().collect()
    }

    /// Get mutable reference to session (for run_loop / TUI).
    pub fn session(&self) -> &Session<Msg> {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session<Msg> {
        &mut self.session
    }

    /// Build message history for LLM call (preserves images for multimodal).
    ///
    /// Injects ephemeral project map after system messages (not stored in session).
    /// - First call: full repomap (all top files with symbols)
    /// - Subsequent calls: compact summary + detailed symbols for changed files only
    fn build_history(&mut self) -> Vec<Msg> {
        self.step_count += 1;

        let msgs: Vec<Msg> = self.session.messages().to_vec();

        // Find where system messages end to insert repomap there
        let insert_at = msgs
            .iter()
            .rposition(|m| m.role == "system")
            .map(|i| i + 1)
            .unwrap_or(0);

        let root = Path::new(".");
        let map_content = if self.step_count <= 1 {
            let repomap = solograph::generate_repomap(root);
            format!(
                "## Project Map (full, auto-generated)\n```\n{}\n```",
                repomap
            )
        } else {
            let changed = git_changed_files();
            let context_map = solograph::generate_context_map(root, &changed);
            format!(
                "## Project Map (compact, {} changed files)\n```\n{}\n```",
                changed.len(),
                context_map
            )
        };

        let mut result = Vec::with_capacity(msgs.len() + 1);
        result.extend_from_slice(&msgs[..insert_at]);
        result.push(Msg::new(Role::system(), map_content));
        result.extend_from_slice(&msgs[insert_at..]);
        result
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.session.push(Role::user(), content.into());
    }

    /// Add a user message with inline images (for multimodal input).
    pub fn add_user_message_with_images(
        &mut self,
        content: impl Into<String>,
        images: Vec<sgr_agent::ImagePart>,
    ) {
        let mut msg = Msg::new(Role::user(), content.into());
        msg.images = images;
        self.session.push_msg(msg);
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        self.session.push(Role::assistant(), content.into());
    }

    /// Add a tool result with call_id for Responses API stateful chaining.
    pub fn add_tool_result(&mut self, content: impl Into<String>, call_id: Option<String>) {
        let mut msg = Msg::new(Role::tool(), content.into());
        msg.call_id = call_id;
        self.session.push_msg(msg);
    }

    /// Reset stateful chaining (e.g. after compaction changes message history).
    pub fn reset_response_chain(&self) {
        *self.last_response_id.lock().unwrap() = None;
    }

    /// Call LLM and get next step (non-streaming).
    /// Uses Responses API stateful chaining for prompt caching.
    pub async fn step(&mut self) -> Result<SgrNextStep> {
        // Try LLM compaction before falling back to simple trim
        self.try_compact().await;
        self.session.trim();
        let history = self.build_history();

        let input_chars: usize = history.iter().map(|m| m.content.len()).sum();
        self.last_input_chars = input_chars;

        let provider = self.provider.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "No LLM provider configured. Set model in config.toml or use --model flag."
            )
        })?;

        let prev_id = self.last_response_id.lock().unwrap().clone();
        let mut sgr_msgs = backend::msgs_to_sgr_messages(&history);
        sgr_msgs.insert(0, sgr_agent::Message::system(SGR_SYSTEM_PROMPT));

        let step = provider
            .call_flexible(&sgr_msgs, prev_id.as_deref())
            .await?;

        // Store response_id for next call's chaining
        *self.last_response_id.lock().unwrap() = step.response_id.clone();

        Ok(step)
    }

    /// Try LLM-based context compaction if history is getting large.
    /// Falls through silently on error — trim_messages will handle it as fallback.
    async fn try_compact(&mut self) {
        let provider = match &self.provider {
            Some(p) => p,
            None => return,
        };

        // Convert session messages to sgr-agent Messages for compaction
        let msgs: Vec<sgr_agent::Message> = self
            .session
            .messages()
            .iter()
            .map(|m| {
                let role = match m.role.as_str() {
                    "system" => sgr_agent::types::Role::System,
                    "assistant" => sgr_agent::types::Role::Assistant,
                    "tool" => sgr_agent::types::Role::Tool,
                    _ => sgr_agent::types::Role::User,
                };
                let msg = match role {
                    sgr_agent::types::Role::System => sgr_agent::Message::system(&m.content),
                    sgr_agent::types::Role::User if !m.images.is_empty() => {
                        sgr_agent::Message::user_with_images(&m.content, m.images.clone())
                    }
                    sgr_agent::types::Role::User => sgr_agent::Message::user(&m.content),
                    sgr_agent::types::Role::Assistant => sgr_agent::Message::assistant(&m.content),
                    sgr_agent::types::Role::Tool => sgr_agent::Message::tool("", &m.content),
                };
                msg
            })
            .collect();

        let compactor = sgr_agent::compaction::Compactor::new(80_000).with_keep(2, 10);
        if !compactor.needs_compaction(&msgs) {
            return;
        }

        tracing::info!("Context compaction triggered ({} messages)", msgs.len());

        let client = provider.make_compaction_client();

        let mut sgr_msgs = msgs;
        match compactor.compact(client.as_ref(), &mut sgr_msgs).await {
            Ok(true) => {
                // Replace session messages with compacted version
                let compacted: Vec<Msg> = sgr_msgs
                    .iter()
                    .map(|m| Msg {
                        role: match m.role {
                            sgr_agent::types::Role::System => "system".into(),
                            sgr_agent::types::Role::Assistant => "assistant".into(),
                            sgr_agent::types::Role::Tool => "tool".into(),
                            sgr_agent::types::Role::User => "user".into(),
                        },
                        content: m.content.clone(),
                        images: m.images.clone(),
                        call_id: None,
                    })
                    .collect();
                let session_msgs = self.session.messages_mut();
                session_msgs.clear();
                session_msgs.extend(compacted);
                // Compaction changes message history — break stateful chain
                self.reset_response_chain();
                tracing::info!(
                    "Context compacted to {} messages",
                    self.session.messages().len()
                );
            }
            Ok(false) => {} // not needed after all
            Err(e) => {
                tracing::warn!("Compaction failed: {}, falling back to trim", e);
            }
        }
    }

    /// Record output size after LLM response (call from TUI/headless after getting response).
    pub fn record_step_cost(&self, output_text: &str) {
        cost::record_step(self.last_input_chars, output_text.len());
    }

    /// Reset step count (e.g. when loading a new session).
    pub fn reset_step_count(&mut self) {
        self.step_count = 0;
        cost::reset_cost();
    }

    /// Set LLM provider.
    pub fn set_provider(&mut self, provider: LlmProvider) {
        self.provider = Some(provider.clone());
        // Rebuild tool registry with updated provider (for spawn_agent)
        self.tool_registry = Self::build_tool_registry(
            self.state.clone(),
            self.swarm.clone(),
            self.delegate_mgr.clone(),
            self.api_registry.clone(),
            self.mcp.clone(),
            Some(provider),
        );
    }

    /// Get current cost stats for display in TUI.
    pub fn cost_status(&self) -> String {
        cost::session_stats().status_line()
    }

    /// Execute a single action. Returns tool output + done flag.
    /// Auto-checkpoints before mutating actions (write, edit, bash).
    /// Dispatches through ToolRegistry -- each SgrAction variant maps to a Tool impl.
    pub async fn execute_action(&self, action: &Action) -> Result<ActionResult> {
        let sig = Self::action_signature(action);
        if is_mutating_action(&sig) {
            if let Some(label) = create_checkpoint(self.step_count, &sig) {
                tracing::info!("Checkpoint: {}", label);
            }
        }

        // Convert SgrAction to (tool_name, args_json) and dispatch through registry
        let (tool_name, args_json) = action_to_tool_call(action);
        let tool = self
            .tool_registry
            .get(&tool_name)
            .ok_or_else(|| anyhow::anyhow!("Unknown tool: {}", tool_name))?;

        let mut ctx = sgr_agent::context::AgentContext::new();
        match tool.execute(args_json, &mut ctx).await {
            Ok(output) => Ok(ActionResult {
                output: output.content,
                done: output.done || output.waiting,
            }),
            Err(e) => Err(anyhow::anyhow!("{}", e)),
        }
    }
}

/// Convert an SgrAction enum variant into (tool_name, serde_json::Value args).
/// This bridges the legacy SgrAction wire format with the Tool trait dispatch.
fn action_to_tool_call(action: &SgrAction) -> (String, serde_json::Value) {
    // Serialize via serde -- SgrAction is tagged with #[serde(tag = "tool_name")]
    // so serializing gives us {"tool_name": "read_file", "path": "...", ...}
    // We strip "tool_name" to get pure args.
    let mut val = serde_json::to_value(action).unwrap_or_default();
    let tool_name = val
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    if let Some(obj) = val.as_object_mut() {
        obj.remove("tool_name");
    }
    (tool_name, val)
}

/// SgrAgent implementation — used by run_loop_stream in headless mode.
///
/// `execute` delegates to `execute_action` which takes `&self` (no mutation).
/// `decide` calls the SGR provider directly from the passed-in messages.
impl SgrAgent for Agent {
    type Action = Action;
    type Msg = Msg;
    type Error = anyhow::Error;

    async fn decide(&self, messages: &[Msg]) -> Result<StepDecision<Action>> {
        let input_chars: usize = messages.iter().map(|m| m.content.len()).sum();

        let provider = self
            .provider
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No LLM provider configured"))?;

        let prev_id = self.last_response_id.lock().unwrap().clone();
        let mut sgr_msgs = backend::msgs_to_sgr_messages(messages);
        sgr_msgs.insert(0, sgr_agent::Message::system(SGR_SYSTEM_PROMPT));
        let step = provider
            .call_flexible(&sgr_msgs, prev_id.as_deref())
            .await?;

        // Store response_id for next call's chaining
        *self.last_response_id.lock().unwrap() = step.response_id.clone();

        // Track cost
        let output_chars = step.situation.len()
            + step.task.iter().map(|t| t.len()).sum::<usize>()
            + format!("{:?}", step.actions).len();
        cost::record_step(input_chars, output_chars);

        let done = step
            .actions
            .iter()
            .any(|a| matches!(a, Action::Finish { .. } | Action::AskUser { .. }));

        let action_kinds: Vec<ActionKind> = step.actions.iter().map(action_kind).collect();
        let mcp_names: Vec<&str> = match self.mcp.as_ref().as_ref() {
            Some(m) => m.server_names(),
            None => vec![],
        };

        let ctx = HintContext {
            intent: self.intent,
            action_kinds: &action_kinds,
            step_num: self.step_count + 1,
            mcp_servers: &mcp_names,
        };

        let hints = collect_hints(&ctx, &step.actions, action_kind, &self.hint_sources);

        Ok(StepDecision {
            situation: step.situation,
            task: step.task,
            completed: done,
            actions: step.actions,
            hints,
            call_ids: step.call_ids,
        })
    }

    async fn execute(&self, action: &Action) -> Result<ActionResult> {
        self.execute_action(action).await
    }

    fn action_category(action: &Action) -> String {
        match action {
            // For apply_patch: category = target files, not content hash.
            // This way repeated patches on same file(s) trigger loop detection
            // even when patch content differs slightly each time.
            Action::ApplyPatch { patch } => {
                let mut files: Vec<&str> = Vec::new();
                for line in patch.lines() {
                    if let Some(rest) = line
                        .strip_prefix("*** Add File: ")
                        .or_else(|| line.strip_prefix("*** Update File: "))
                        .or_else(|| line.strip_prefix("*** Delete File: "))
                    {
                        files.push(rest.trim());
                    }
                }
                if files.is_empty() {
                    "apply_patch".into()
                } else {
                    files.sort();
                    format!("apply_patch:{}", files.join(","))
                }
            }
            other => sgr_agent::loop_detect::normalize_signature(&Self::action_signature(other)),
        }
    }

    fn action_signature(action: &Action) -> String {
        match action {
            Action::ReadFile { path, .. } => format!("read:{}", path),
            Action::WriteFile { path, content } => {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                content.hash(&mut hasher);
                format!("write:{}:{:x}", path, hasher.finish())
            }
            Action::EditFile {
                path,
                old_string,
                new_string,
            } => {
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                old_string.hash(&mut hasher);
                new_string.hash(&mut hasher);
                format!("edit:{}:{:x}", path, hasher.finish())
            }
            Action::Bash { command, .. } => format!("bash:{}", command),
            Action::BashBg { name, .. } => format!("bg:{}", name),
            Action::SearchCode { query } => format!("search:{}", query),
            Action::GitStatus { .. } => "git_status".into(),
            Action::GitDiff { path, .. } => format!("diff:{:?}", path),
            Action::GitAdd { paths } => format!("add:{:?}", paths),
            Action::GitCommit { message } => format!("commit:{}", message),
            Action::OpenEditor { path, .. } => format!("open:{}", path),
            Action::AskUser { question } => format!("ask:{}", question),
            Action::Finish { summary } => format!("finish:{}", summary),
            Action::Memory {
                operation, section, ..
            } => format!("memory:{}:{:?}", operation, section),
            Action::McpCall { server, tool, .. } => format!("mcp:{}:{}", server, tool),
            Action::ProjectMap { path } => format!("project_map:{:?}", path),
            Action::Dependencies { path } => format!("deps:{:?}", path),
            Action::Task {
                operation, task_id, ..
            } => format!("task:{}:{:?}", operation, task_id),
            Action::ApplyPatch { patch } => {
                // Include hash of patch content so different patches aren't treated as loops
                use std::hash::{Hash, Hasher};
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                patch.hash(&mut hasher);
                format!("apply_patch:{:x}", hasher.finish())
            }
            Action::SpawnAgent { role, task, .. } => {
                format!("spawn:{}:{}", role, &task[..task.len().min(40)])
            }
            Action::WaitAgents { agent_ids, .. } => format!("wait:{:?}", agent_ids),
            Action::AgentStatus { agent_id } => format!("status:{:?}", agent_id),
            Action::CancelAgent { agent_id } => format!("cancel:{}", agent_id),
            Action::Api {
                action, api_name, ..
            } => format!("api:{}:{}", action, api_name.as_deref().unwrap_or("?")),
            Action::DelegateTask {
                agent,
                task,
                task_path,
                ..
            } => {
                let label = task_path.as_deref().or(task.as_deref()).unwrap_or("?");
                format!("delegate:{}:{}", agent, &label[..label.len().min(40)])
            }
            Action::DelegateStatus { id } => format!("delegate_status:{:?}", id),
            Action::DelegateResult { id } => format!("delegate_result:{}", id),
        }
    }
}

/// Classify action into coarse ActionKind for intent guard.
pub fn action_kind(action: &Action) -> ActionKind {
    match action {
        Action::ReadFile { .. }
        | Action::SearchCode { .. }
        | Action::GitStatus { .. }
        | Action::GitDiff { .. }
        | Action::ProjectMap { .. }
        | Action::Dependencies { .. } => ActionKind::Read,
        Action::WriteFile { .. }
        | Action::EditFile { .. }
        | Action::OpenEditor { .. }
        | Action::ApplyPatch { .. } => ActionKind::Write,
        Action::Bash { .. } | Action::BashBg { .. } => ActionKind::Execute,
        Action::GitAdd { .. } | Action::GitCommit { .. } => ActionKind::GitMutate,
        Action::AskUser { .. }
        | Action::Finish { .. }
        | Action::Memory { .. }
        | Action::Task { .. } => ActionKind::Plan,
        Action::McpCall { .. } => ActionKind::External,
        Action::SpawnAgent { .. }
        | Action::WaitAgents { .. }
        | Action::AgentStatus { .. }
        | Action::CancelAgent { .. } => ActionKind::Execute,
        Action::Api { .. } => ActionKind::External,
        Action::DelegateTask { .. } => ActionKind::Execute,
        Action::DelegateStatus { .. } | Action::DelegateResult { .. } => ActionKind::Read,
    }
}

impl SgrAgentStream for Agent {
    fn decide_stream<T>(
        &self,
        messages: &[Msg],
        mut on_token: T,
    ) -> impl std::future::Future<Output = Result<StepDecision<Action>>> + Send
    where
        T: FnMut(&str) + Send,
    {
        let sgr_msgs_base = backend::msgs_to_sgr_messages(messages);
        let input_chars: usize = messages.iter().map(|m| m.content.len()).sum();
        let provider = self.provider.clone();
        let prev_id = self.last_response_id.lock().unwrap().clone();
        async move {
            let provider = provider.ok_or_else(|| anyhow::anyhow!("No LLM provider configured"))?;

            let mut sgr_msgs = sgr_msgs_base;
            sgr_msgs.insert(0, sgr_agent::Message::system(SGR_SYSTEM_PROMPT));
            let step = provider
                .call_flexible(&sgr_msgs, prev_id.as_deref())
                .await?;

            // Store response_id for next call's chaining
            *self.last_response_id.lock().unwrap() = step.response_id.clone();
            // Emit situation as single token (no streaming yet)
            on_token(&step.situation);

            // Track cost
            let output_chars = step.situation.len()
                + step.task.iter().map(|t| t.len()).sum::<usize>()
                + format!("{:?}", step.actions).len();
            cost::record_step(input_chars, output_chars);
            let done = step
                .actions
                .iter()
                .any(|a| matches!(a, Action::Finish { .. } | Action::AskUser { .. }));

            let action_kinds: Vec<ActionKind> = step.actions.iter().map(action_kind).collect();
            let mcp_names: Vec<&str> = match self.mcp.as_ref().as_ref() {
                Some(m) => m.server_names(),
                None => vec![],
            };

            let ctx = HintContext {
                intent: self.intent,
                action_kinds: &action_kinds,
                step_num: self.step_count + 1,
                mcp_servers: &mcp_names,
            };

            let hints = collect_hints(&ctx, &step.actions, action_kind, &self.hint_sources);

            Ok(StepDecision {
                situation: step.situation,
                task: step.task,
                completed: done,
                actions: step.actions,
                hints,
                call_ids: step.call_ids,
            })
        }
    }
}

/// Get list of changed files from git (modified + untracked, relative paths).
fn git_changed_files() -> Vec<String> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain", "-u"])
        .output();
    let Ok(output) = output else {
        return vec![];
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            // porcelain format: "XY path" or "XY path -> renamed"
            let path = line.get(3..)?.split(" -> ").next()?;
            let trimmed = path.trim();
            if trimmed.is_empty() {
                return None;
            }
            Some(trimmed.to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_agent_creates_session_dir() {
        let _agent = Agent::new();
        assert!(std::path::Path::new(".rust-code").exists());
    }

    #[test]
    fn add_messages_to_history() {
        let mut agent = Agent::new();
        let initial = agent.history().len();

        agent.add_user_message("hello");
        assert_eq!(agent.history().len(), initial + 1);
        assert_eq!(agent.history().last().unwrap().role, "user");
        assert_eq!(agent.history().last().unwrap().content, "hello");

        agent.add_assistant_message("world");
        assert_eq!(agent.history().len(), initial + 2);
        assert_eq!(agent.history().last().unwrap().role, "assistant");
    }

    #[test]
    fn session_file_written() {
        let mut agent = Agent::new();
        agent.add_user_message("test persistence");

        let content = std::fs::read_to_string(agent.session.session_file()).unwrap();
        assert!(content.contains("test persistence"));
    }

    #[test]
    fn loop_detector_works() {
        let mut ld = Agent::new_loop_detector();
        use sgr_agent::LoopStatus;
        assert_eq!(ld.check("a"), LoopStatus::Ok);
        assert_eq!(ld.check("a"), LoopStatus::Ok);
        assert_eq!(ld.check("a"), LoopStatus::Ok);
        assert_eq!(ld.check("a"), LoopStatus::Ok);
        assert_eq!(ld.check("a"), LoopStatus::Warning(5));
    }

    #[test]
    fn agent_context_loaded_from_home() {
        let dir = std::env::temp_dir().join("rc_test_agent_home");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SOUL.md"), "Be direct.").unwrap();
        std::fs::write(dir.join("IDENTITY.md"), "Name: test-agent").unwrap();

        let ctx = sgr_agent::MemoryContext::load(dir.to_str().unwrap());
        assert_eq!(ctx.parts.len(), 2);
        let msg = ctx.to_system_message().unwrap();
        assert!(msg.contains("Be direct"));
        assert!(msg.contains("test-agent"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
