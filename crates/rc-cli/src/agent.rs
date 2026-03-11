use crate::backend::{self, SgrAction, SgrNextStep, SgrProvider};
use crate::tools::{
    FuzzySearcher, build_skills_context, cost, create_checkpoint, git_add, git_commit, git_diff,
    git_status, is_mutating_action, mcp::McpManager, read_file, truncate_output, write_file,
};
use anyhow::Result;
use baml_agent::{
    ActionKind, ActionResult, AgentMessage, HintContext, Intent, LoopDetector, MessageRole,
    Session, SgrAgent, SgrAgentStream, StepDecision, collect_hints,
};
use sgr_agent::swarm::{AgentId, AgentRole, SwarmManager};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

/// Action type — the 18-variant tool enum.
pub type Action = SgrAction;

// Implement baml-agent traits for BAML's generated Message type

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
}

impl AgentMessage for Msg {
    type Role = Role;
    fn new(role: Role, content: String) -> Self {
        Self {
            role: role.0,
            content,
        }
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
    mcp: Option<McpManager>,
    step_count: usize,
    last_input_chars: usize,
    /// LLM provider (SGR pure Rust HTTP).
    provider: Option<SgrProvider>,
    /// Current user intent for action filtering.
    pub intent: Intent,
    /// Pluggable hint sources.
    hint_sources: Vec<Box<dyn baml_agent::HintSource>>,
    /// Persistent CWD for bash commands (tracks `cd` across steps).
    /// Interior mutability: execute() takes &self but needs to update CWD.
    cwd: std::sync::Mutex<std::path::PathBuf>,
    /// Track consecutive edit failures per file path for fallback hints.
    edit_failures: std::sync::Mutex<std::collections::HashMap<String, usize>>,
    /// Cache of recently read files — prevents wasteful re-reads.
    /// Key: resolved path, Value: (content, step_number).
    read_cache: std::sync::Mutex<std::collections::HashMap<String, (String, usize)>>,
    /// Multi-agent swarm manager for sub-agents.
    swarm: Arc<TokioMutex<SwarmManager>>,
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
        let mut ctx = baml_agent::AgentContext::load(AGENT_HOME);
        ctx.merge(baml_agent::AgentContext::load_project(Path::new(".")));
        if let Some(msg) = ctx.to_system_message() {
            session.push(Role::system(), msg);
        }

        // Inject installed skills context
        if let Some(skills_ctx) = build_skills_context() {
            session.push(Role::system(), skills_ctx);
        }

        Self {
            session,
            mcp: None,
            step_count: 0,
            last_input_chars: 0,
            provider: None,
            intent: Intent::Auto,
            hint_sources: baml_agent::default_sources_with_tasks(Path::new(".")),
            cwd: std::sync::Mutex::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
            edit_failures: std::sync::Mutex::new(std::collections::HashMap::new()),
            read_cache: std::sync::Mutex::new(std::collections::HashMap::new()),
            swarm: Arc::new(TokioMutex::new(SwarmManager::new())),
        }
    }

    /// Set working directory for tool execution.
    pub fn set_cwd(&self, path: std::path::PathBuf) {
        *self.cwd.lock().unwrap() = path;
    }

    /// Resolve a potentially relative path against agent CWD.
    fn resolve_path(&self, path: &str) -> String {
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            path.to_string()
        } else {
            let cwd = self.cwd.lock().unwrap();
            cwd.join(p).to_string_lossy().to_string()
        }
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
        self.mcp = Some(manager);
        Ok(())
    }

    pub fn mcp(&self) -> Option<&McpManager> {
        self.mcp.as_ref()
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
    pub fn session_mut(&mut self) -> &mut Session<Msg> {
        &mut self.session
    }

    /// Build message history for LLM call as (role, content) pairs.
    ///
    /// Injects ephemeral project map after system messages (not stored in session).
    /// - First call: full repomap (all top files with symbols)
    /// - Subsequent calls: compact summary + detailed symbols for changed files only
    fn build_history(&mut self) -> Vec<(String, String)> {
        self.step_count += 1;

        let msgs: Vec<_> = self
            .session
            .messages()
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();

        // Find where system messages end to insert repomap there
        let insert_at = msgs
            .iter()
            .rposition(|(role, _)| role == "system")
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
        result.push(("system".into(), map_content));
        result.extend_from_slice(&msgs[insert_at..]);
        result
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.session.push(Role::user(), content.into());
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        self.session.push(Role::assistant(), content.into());
    }

    /// Call LLM and get next step (non-streaming).
    pub async fn step(&mut self) -> Result<SgrNextStep> {
        // Try LLM compaction before falling back to simple trim
        self.try_compact().await;
        self.session.trim();
        let history = self.build_history();

        let input_chars: usize = history.iter().map(|(_, c)| c.len()).sum();
        self.last_input_chars = input_chars;

        let provider = self.provider.as_ref().ok_or_else(|| {
            anyhow::anyhow!("No LLM provider configured. Use --sgr or set GEMINI_API_KEY.")
        })?;

        let mut sgr_msgs = backend::to_sgr_messages(&history);
        sgr_msgs.insert(0, sgr_agent::Message::system(SGR_SYSTEM_PROMPT));

        provider.call_flexible(&sgr_msgs).await
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
                match role {
                    sgr_agent::types::Role::System => sgr_agent::Message::system(&m.content),
                    sgr_agent::types::Role::User => sgr_agent::Message::user(&m.content),
                    sgr_agent::types::Role::Assistant => sgr_agent::Message::assistant(&m.content),
                    sgr_agent::types::Role::Tool => sgr_agent::Message::tool("", &m.content),
                }
            })
            .collect();

        let compactor = sgr_agent::compaction::Compactor::new(80_000).with_keep(2, 10);
        if !compactor.needs_compaction(&msgs) {
            return;
        }

        tracing::info!("Context compaction triggered ({} messages)", msgs.len());

        let client = match provider.make_compaction_client().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Compaction client error: {}, falling back to trim", e);
                return;
            }
        };

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
                    })
                    .collect();
                let session_msgs = self.session.messages_mut();
                session_msgs.clear();
                session_msgs.extend(compacted);
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
    pub fn set_provider(&mut self, provider: SgrProvider) {
        self.provider = Some(provider);
    }

    /// Get current cost stats for display in TUI.
    pub fn cost_status(&self) -> String {
        cost::session_stats().status_line()
    }

    /// Execute a single action. Returns tool output + done flag.
    /// Auto-checkpoints before mutating actions (write, edit, bash).
    pub async fn execute_action(&self, action: &Action) -> Result<ActionResult> {
        let sig = Self::action_signature(action);
        if is_mutating_action(&sig) {
            if let Some(label) = create_checkpoint(self.step_count, &sig) {
                tracing::info!("Checkpoint: {}", label);
            }
        }
        match action {
            SgrAction::ReadFile {
                path,
                offset,
                limit,
            } => {
                let resolved = self.resolve_path(path);
                // Check read cache — return cached content with warning on re-read
                let cache_key = resolved.clone();
                let is_reread = {
                    let cache = self.read_cache.lock().unwrap();
                    cache.contains_key(&cache_key)
                };
                let content = read_file(
                    &resolved,
                    offset.map(|o| o as usize),
                    limit.map(|l| l as usize),
                )
                .await?;
                let output = if is_reread {
                    // Return truncated content on re-read to save context window
                    let lines: Vec<&str> = content.lines().collect();
                    let preview = if lines.len() > 5 {
                        format!(
                            "{}\n... ({} more lines — use content from conversation history)",
                            lines[..5].join("\n"),
                            lines.len() - 5
                        )
                    } else {
                        content.clone()
                    };
                    format!(
                        "⚠ RE-READ: You already read this file. Content unchanged. \
                         STOP re-reading and ACT on what you already know.\n\
                         Preview (first 5 lines):\n{}",
                        preview
                    )
                } else {
                    format!("File contents of {}:\n{}", path, content)
                };
                // Update cache
                {
                    let mut cache = self.read_cache.lock().unwrap();
                    cache.insert(cache_key, (content, self.step_count));
                }
                Ok(ActionResult {
                    output: truncate_output(&output),
                    done: false,
                })
            }
            SgrAction::WriteFile { path, content } => {
                let resolved = self.resolve_path(path);
                let is_new = !std::path::Path::new(&resolved).exists();
                write_file(&resolved, content).await?;
                // Invalidate read cache for this file
                self.read_cache.lock().unwrap().remove(&resolved);
                let label = if is_new { "Created" } else { "Wrote" };
                let lines = content.lines().count();
                Ok(ActionResult {
                    output: format!("{} {} ({} lines)", label, path, lines),
                    done: false,
                })
            }
            SgrAction::EditFile {
                path,
                old_string,
                new_string,
            } => {
                let resolved = self.resolve_path(path);
                match crate::tools::edit_file(&resolved, old_string, new_string).await {
                    Ok(()) => {
                        // Reset failure counter and invalidate read cache on success
                        self.edit_failures.lock().unwrap().remove(path.as_str());
                        self.read_cache.lock().unwrap().remove(&resolved);
                        let old_lines: Vec<&str> = old_string.lines().collect();
                        let new_lines: Vec<&str> = new_string.lines().collect();
                        let mut diff = format!(
                            "Edited {} ({}→{} lines)\n",
                            path,
                            old_lines.len(),
                            new_lines.len()
                        );
                        for l in &old_lines {
                            diff.push_str(&format!("- {}\n", l));
                        }
                        for l in &new_lines {
                            diff.push_str(&format!("+ {}\n", l));
                        }
                        Ok(ActionResult {
                            output: diff,
                            done: false,
                        })
                    }
                    Err(e) => {
                        let count = {
                            let mut failures = self.edit_failures.lock().unwrap();
                            let c = failures.entry(path.to_string()).or_insert(0);
                            *c += 1;
                            *c
                        };
                        let mut err_msg = format!("{}", e);
                        if count >= 2 {
                            err_msg.push_str(&format!(
                                "\n\n⚠ edit_file has failed {} times on this file. \
                                 STOP trying edit_file. Instead: use read_file to get the EXACT current content, \
                                 then use write_file with the complete modified content.",
                                count
                            ));
                        }
                        Err(anyhow::anyhow!("{}", err_msg))
                    }
                }
            }
            SgrAction::ApplyPatch { patch } => {
                let current_cwd = self.cwd.lock().unwrap().clone();
                match baml_agent::tools::apply_patch::apply_patch_to_files(patch, &current_cwd)
                    .await
                {
                    Ok(result) => {
                        let mut summary = String::new();
                        for p in &result.added {
                            summary.push_str(&format!("A {}\n", p.display()));
                        }
                        for p in &result.modified {
                            summary.push_str(&format!("M {}\n", p.display()));
                        }
                        for p in &result.deleted {
                            summary.push_str(&format!("D {}\n", p.display()));
                        }
                        if summary.is_empty() {
                            summary = "Patch applied (no changes).".to_string();
                        }

                        // Show updated content so agent has fresh state for subsequent patches.
                        // Limit: first 3 files, max 200 lines each.
                        let changed: Vec<&std::path::Path> = result
                            .modified
                            .iter()
                            .chain(result.added.iter())
                            .take(3)
                            .map(|p| p.as_path())
                            .collect();
                        for p in &changed {
                            let abs = if p.is_absolute() {
                                p.to_path_buf()
                            } else {
                                current_cwd.join(p)
                            };
                            if let Ok(content) = tokio::fs::read_to_string(&abs).await {
                                let lines: Vec<&str> = content.lines().collect();
                                let display = if lines.len() > 200 {
                                    format!(
                                        "{}\n... ({} more lines)",
                                        lines[..200].join("\n"),
                                        lines.len() - 200
                                    )
                                } else {
                                    content
                                };
                                summary.push_str(&format!(
                                    "\n--- Updated {} ---\n{}\n",
                                    p.display(),
                                    display
                                ));
                            }
                        }

                        // Invalidate read cache for changed files
                        {
                            let mut cache = self.read_cache.lock().unwrap();
                            for p in result
                                .modified
                                .iter()
                                .chain(result.added.iter())
                                .chain(result.deleted.iter())
                            {
                                let key = p.to_string_lossy().to_string();
                                cache.remove(&key);
                                // Also remove with cwd prefix
                                let abs = current_cwd.join(p);
                                cache.remove(&abs.to_string_lossy().to_string());
                            }
                        }

                        Ok(ActionResult {
                            output: summary,
                            done: false,
                        })
                    }
                    Err(e) => Err(anyhow::anyhow!(
                        "apply_patch error: {}\n\n\
                         IMPORTANT: If context lines don't match, use read_file FIRST to see the current file content, then retry.\n\n\
                         CORRECT FORMAT:\n\
                         *** Begin Patch\n\
                         *** Update File: path/to/file.ts\n\
                         @@ function_name\n\
                          context line (must match file exactly)\n\
                         -old line\n\
                         +new line\n\
                          context line\n\
                         *** End Patch\n\n\
                         Do NOT use unified diff (@@ -N,N +N,N @@). Use *** Add/Update/Delete File: headers.",
                        e
                    )),
                }
            }
            SgrAction::Bash {
                command, timeout, ..
            } => {
                let timeout_ms = timeout.map(|t| (t as u64).min(600_000));
                let current_cwd = self.cwd.lock().unwrap().clone();
                let result = crate::tools::run_command_in(command, &current_cwd, timeout_ms).await;
                *self.cwd.lock().unwrap() = result.cwd;
                let output_text = if result.exit_code == 0 {
                    if result.output.trim().is_empty() {
                        "Command completed successfully (no output).".to_string()
                    } else {
                        truncate_output(&format!("Command output:\n{}", result.output))
                    }
                } else {
                    truncate_output(&format!(
                        "Command output:\n{}\n[exit code: {}]",
                        result.output, result.exit_code
                    ))
                };
                Ok(ActionResult {
                    output: output_text,
                    done: false,
                })
            }
            SgrAction::BashBg { name, command } => {
                let output = crate::tools::run_command_bg(name, command).await?;
                Ok(ActionResult {
                    output: format!("[BG] {}", output),
                    done: false,
                })
            }
            SgrAction::SearchCode { query } => {
                let mut result = String::new();

                if let Ok(files) = FuzzySearcher::get_all_files().await {
                    let mut searcher = FuzzySearcher::new();
                    let matches = searcher.fuzzy_match_files(query, &files);
                    if !matches.is_empty() {
                        result.push_str(&format!("File path matches for '{}':\n", query));
                        for (score, path) in matches.iter().take(5) {
                            if *score > 50 {
                                result.push_str(&format!("- {}\n", path));
                            }
                        }
                        result.push('\n');
                    }
                }

                result.push_str(&format!("Content search results for '{}':\n", query));
                let safe_query = query.replace("'", "'\\''");
                let search_cmd = format!("rg -n '{}' . || grep -rn '{}' .", safe_query, safe_query);
                let current_cwd = self.cwd.lock().unwrap().clone();
                let search_result =
                    crate::tools::run_command_in(&search_cmd, &current_cwd, None).await;
                let output = &search_result.output;
                if search_result.exit_code == 0 && !output.trim().is_empty() {
                    let lines: Vec<&str> = output.lines().collect();
                    if lines.len() > 100 {
                        result.push_str(&lines[..100].join("\n"));
                        result.push_str(&format!(
                            "\n...[Truncated {} more lines]...",
                            lines.len() - 100
                        ));
                    } else {
                        result.push_str(output);
                    }
                } else {
                    result.push_str("No content matches found.");
                }

                Ok(ActionResult {
                    output: truncate_output(&result),
                    done: false,
                })
            }
            SgrAction::GitStatus { .. } => match git_status()? {
                Some(status) => {
                    let mut result = format!(
                        "Git Status:\nBranch: {}\nDirty: {}\n",
                        status.branch, status.dirty
                    );
                    if !status.modified_files.is_empty() {
                        result.push_str("\nModified files:\n");
                        for f in &status.modified_files {
                            result.push_str(&format!("  - {}\n", f));
                        }
                    }
                    if !status.staged_files.is_empty() {
                        result.push_str("\nStaged files:\n");
                        for f in &status.staged_files {
                            result.push_str(&format!("  + {}\n", f));
                        }
                    }
                    if !status.untracked_files.is_empty() {
                        result.push_str("\nUntracked files:\n");
                        for f in &status.untracked_files {
                            result.push_str(&format!("  ? {}\n", f));
                        }
                    }
                    Ok(ActionResult {
                        output: result,
                        done: false,
                    })
                }
                None => Ok(ActionResult {
                    output: "Not in a git repository".into(),
                    done: false,
                }),
            },
            SgrAction::GitDiff { path, cached } => {
                let diff = git_diff(path.as_deref(), cached.unwrap_or(false))?;
                let output = if diff.is_empty() {
                    "No changes to show".into()
                } else {
                    format!("Git Diff:\n{}", diff)
                };
                Ok(ActionResult {
                    output,
                    done: false,
                })
            }
            SgrAction::GitAdd { paths } => {
                git_add(paths)?;
                Ok(ActionResult {
                    output: format!("Added {} files to staging", paths.len()),
                    done: false,
                })
            }
            SgrAction::GitCommit { message } => {
                git_commit(message)?;
                Ok(ActionResult {
                    output: format!("Committed: {}", message),
                    done: false,
                })
            }
            SgrAction::OpenEditor { path, .. } => Ok(ActionResult {
                output: format!("Opened {} in editor", path),
                done: false,
            }),
            SgrAction::Finish { summary } => Ok(ActionResult {
                output: format!("Task finished: {}", summary),
                done: true,
            }),
            SgrAction::AskUser { question } => Ok(ActionResult {
                output: format!("Question for user: {}", question),
                done: true,
            }),
            SgrAction::Memory {
                operation,
                category,
                section,
                content,
                context,
                confidence,
            } => {
                let memory_path = Path::new(AGENT_HOME).join("MEMORY.jsonl");
                let op = operation.to_lowercase();
                let cat = category.as_deref().unwrap_or("insight").to_lowercase();
                let conf = confidence.as_deref().unwrap_or("tentative").to_lowercase();

                match op.as_str() {
                    "save" => {
                        let sec = section.as_deref().unwrap_or("general");
                        let entry = serde_json::json!({
                            "category": cat,
                            "section": sec,
                            "content": content,
                            "context": context,
                            "confidence": conf,
                            "created": std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default().as_secs(),
                        });
                        let mut file = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open(&memory_path)
                            .map_err(|e| anyhow::anyhow!("Memory write: {}", e))?;
                        use std::io::Write;
                        writeln!(file, "{}", entry)
                            .map_err(|e| anyhow::anyhow!("Memory write: {}", e))?;
                        Ok(ActionResult {
                            output: format!("Memory saved: [{}] {} ({})", cat, sec, conf),
                            done: false,
                        })
                    }
                    "forget" => {
                        let sec = section.as_deref().unwrap_or("general");
                        if memory_path.exists() {
                            let file_content =
                                std::fs::read_to_string(&memory_path).unwrap_or_default();
                            let filtered: Vec<&str> = file_content
                                .lines()
                                .filter(|line| {
                                    serde_json::from_str::<serde_json::Value>(line)
                                        .map(|v| v["section"].as_str() != Some(sec))
                                        .unwrap_or(true)
                                })
                                .collect();
                            let removed = file_content.lines().count() - filtered.len();
                            std::fs::write(&memory_path, filtered.join("\n") + "\n")
                                .map_err(|e| anyhow::anyhow!("Memory write: {}", e))?;
                            Ok(ActionResult {
                                output: format!(
                                    "Memory: forgot {} entries from '{}'",
                                    removed, sec
                                ),
                                done: false,
                            })
                        } else {
                            Ok(ActionResult {
                                output: "Memory: nothing to forget (no entries)".into(),
                                done: false,
                            })
                        }
                    }
                    _ => Ok(ActionResult {
                        output: format!("Unknown memory operation: {}", op),
                        done: false,
                    }),
                }
            }
            SgrAction::McpCall {
                server,
                tool,
                arguments,
            } => {
                let Some(mcp) = &self.mcp else {
                    return Ok(ActionResult {
                        output: "MCP not initialized. No .mcp.json found.".into(),
                        done: false,
                    });
                };
                let args = arguments.as_ref().and_then(|json_str| {
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json_str)
                        .ok()
                });
                match mcp.call_tool(server, tool, args).await {
                    Ok(result) => {
                        let output = crate::tools::mcp::format_tool_result(&result);
                        Ok(ActionResult {
                            output: format!("MCP [{}] {}:\n{}", server, tool, output),
                            done: false,
                        })
                    }
                    Err(e) => Ok(ActionResult {
                        output: format!("MCP Error [{}] {}: {}", server, tool, e),
                        done: false,
                    }),
                }
            }
            SgrAction::ProjectMap { path } => {
                let dir = path.as_deref().unwrap_or(".");
                let map = solograph::generate_repomap(Path::new(dir));
                Ok(ActionResult {
                    output: map,
                    done: false,
                })
            }
            SgrAction::Task {
                operation,
                title,
                task_id,
                status,
                priority,
                notes,
            } => {
                let project_root = Path::new(".");
                let op = operation.to_lowercase();
                match op.as_str() {
                    "create" => {
                        let t = title.as_deref().unwrap_or("Untitled");
                        let pri = priority
                            .as_ref()
                            .and_then(|p| baml_agent::Priority::parse(&p.to_lowercase()))
                            .unwrap_or(baml_agent::Priority::Medium);
                        let mut task = baml_agent::create_task(project_root, t, pri);
                        if let Some(n) = notes {
                            task.body = n.clone();
                            baml_agent::save_task(project_root, &task);
                        }
                        Ok(ActionResult {
                            output: format!(
                                "Created task #{} [{}] ({}): {}",
                                task.id, task.status, task.priority, task.title
                            ),
                            done: false,
                        })
                    }
                    "list" => {
                        let tasks = baml_agent::load_tasks(project_root);
                        if tasks.is_empty() {
                            Ok(ActionResult {
                                output:
                                    "No tasks found. Use task(operation='create') to create one."
                                        .into(),
                                done: false,
                            })
                        } else {
                            let mut output = format!("Tasks ({}):\n", tasks.len());
                            for t in &tasks {
                                output.push_str(&format!(
                                    "  #{} [{}] ({}) {}\n",
                                    t.id, t.status, t.priority, t.title
                                ));
                            }
                            Ok(ActionResult {
                                output,
                                done: false,
                            })
                        }
                    }
                    "update" => {
                        let Some(id) = task_id else {
                            return Ok(ActionResult {
                                output: "Error: task_id required for update".into(),
                                done: false,
                            });
                        };
                        let id = *id as u16;
                        if let Some(status_val) = status {
                            let status_str = status_val.to_lowercase();
                            if let Some(s) = baml_agent::TaskStatus::parse(&status_str) {
                                baml_agent::update_status(project_root, id, s);
                            }
                        }
                        if let Some(n) = notes {
                            baml_agent::append_notes(project_root, id, n);
                        }
                        let tasks = baml_agent::load_tasks(project_root);
                        let task = tasks.iter().find(|t| t.id == id);
                        match task {
                            Some(t) => Ok(ActionResult {
                                output: format!(
                                    "Updated task #{} [{}] ({}): {}",
                                    t.id, t.status, t.priority, t.title
                                ),
                                done: false,
                            }),
                            None => Ok(ActionResult {
                                output: format!("Task #{} not found", id),
                                done: false,
                            }),
                        }
                    }
                    "done" => {
                        let Some(id) = task_id else {
                            return Ok(ActionResult {
                                output: "Error: task_id required for done".into(),
                                done: false,
                            });
                        };
                        match baml_agent::update_status(
                            project_root,
                            *id as u16,
                            baml_agent::TaskStatus::Done,
                        ) {
                            Some(t) => Ok(ActionResult {
                                output: format!("Completed task #{}: {}", t.id, t.title),
                                done: false,
                            }),
                            None => Ok(ActionResult {
                                output: format!("Task #{} not found", id),
                                done: false,
                            }),
                        }
                    }
                    _ => Ok(ActionResult {
                        output: format!("Unknown task operation: {}", op),
                        done: false,
                    }),
                }
            }
            SgrAction::Dependencies { path } => {
                let manifest = if let Some(p) = path {
                    std::path::PathBuf::from(p)
                } else {
                    ["Cargo.toml", "package.json", "pyproject.toml"]
                        .iter()
                        .map(std::path::PathBuf::from)
                        .find(|p| p.exists())
                        .unwrap_or_else(|| std::path::PathBuf::from("Cargo.toml"))
                };
                let deps = solograph::parse_deps(&manifest);
                if deps.is_empty() {
                    Ok(ActionResult {
                        output: format!("No dependencies found in {}", manifest.display()),
                        done: false,
                    })
                } else {
                    let output = deps
                        .iter()
                        .map(|d| {
                            let kind = match d.kind {
                                solograph::DependencyKind::Dev => " [dev]",
                                solograph::DependencyKind::Build => " [build]",
                                solograph::DependencyKind::Normal => "",
                            };
                            format!("  {} = {}{}", d.name, d.version, kind)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(ActionResult {
                        output: format!("Dependencies from {}:\n{}", manifest.display(), output),
                        done: false,
                    })
                }
            }
            SgrAction::SpawnAgent {
                role,
                task,
                max_steps,
            } => {
                use sgr_agent::swarm::SpawnConfig;

                let agent_role = match role.as_str() {
                    "explorer" => AgentRole::Explorer,
                    "worker" => AgentRole::Worker,
                    "reviewer" => AgentRole::Reviewer,
                    other => AgentRole::Custom(other.to_string()),
                };

                let provider = match &self.provider {
                    Some(p) => p,
                    None => {
                        return Ok(ActionResult {
                            output: "Cannot spawn agent: no LLM provider configured.".into(),
                            done: false,
                        });
                    }
                };

                // Create sub-agent's LLM client + agent + tools
                let client = match provider.make_gemini_client().await {
                    Ok(c) => c,
                    Err(e) => {
                        return Ok(ActionResult {
                            output: format!("Failed to create LLM client for sub-agent: {}", e),
                            done: false,
                        });
                    }
                };

                let sub_prompt = format!(
                    "You are a {} sub-agent. Complete the task efficiently. Respond with JSON only.",
                    agent_role.name()
                );
                let sub_agent =
                    sgr_agent::agents::flexible::FlexibleAgent::new(client, sub_prompt, 3);
                let sub_tools = sgr_agent::registry::ToolRegistry::new();

                let mut config = match agent_role {
                    AgentRole::Explorer => SpawnConfig::explorer(task.clone()),
                    AgentRole::Worker => SpawnConfig::worker(task.clone()),
                    AgentRole::Reviewer => SpawnConfig::reviewer(task.clone()),
                    AgentRole::Custom(_) => SpawnConfig::worker(task.clone()),
                };
                if let Some(n) = max_steps {
                    config.max_steps = *n as usize;
                }

                let parent_ctx = sgr_agent::context::AgentContext::new();
                let mut swarm = self.swarm.lock().await;
                match swarm.spawn(config, Box::new(sub_agent), sub_tools, &parent_ctx) {
                    Ok(id) => Ok(ActionResult {
                        output: format!("Spawned {} agent: {}\nTask: {}", agent_role, id, task),
                        done: false,
                    }),
                    Err(e) => Ok(ActionResult {
                        output: format!("Failed to spawn agent: {}", e),
                        done: false,
                    }),
                }
            }
            SgrAction::WaitAgents {
                agent_ids,
                timeout_secs,
            } => {
                let timeout =
                    std::time::Duration::from_secs(timeout_secs.map(|s| s as u64).unwrap_or(300));

                let ids: Vec<AgentId>;
                {
                    let swarm = self.swarm.lock().await;
                    ids = if agent_ids.is_empty() {
                        swarm.all_agent_ids()
                    } else {
                        agent_ids.iter().map(|s| AgentId(s.clone())).collect()
                    };
                }

                if ids.is_empty() {
                    return Ok(ActionResult {
                        output: "No agents to wait for.".into(),
                        done: false,
                    });
                }

                let mut swarm = self.swarm.lock().await;
                let results = swarm.wait_with_timeout(&ids, timeout).await;
                let output = results
                    .iter()
                    .map(|(id, result)| format!("[{}] {}", id, result))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                Ok(ActionResult {
                    output,
                    done: false,
                })
            }
            SgrAction::AgentStatus { agent_id } => {
                let swarm = self.swarm.lock().await;
                let output = if let Some(id) = agent_id {
                    let aid = AgentId(id.clone());
                    match swarm.status(&aid).await {
                        Some(s) => format!("[{}] {}", id, s),
                        None => format!("Agent '{}' not found", id),
                    }
                } else {
                    swarm.status_all_formatted().await
                };
                Ok(ActionResult {
                    output,
                    done: false,
                })
            }
            SgrAction::CancelAgent { agent_id } => {
                let swarm = self.swarm.lock().await;
                if agent_id == "all" {
                    swarm.cancel_all();
                    Ok(ActionResult {
                        output: "Cancelled all agents.".into(),
                        done: false,
                    })
                } else {
                    let aid = AgentId(agent_id.clone());
                    match swarm.cancel(&aid) {
                        Ok(()) => Ok(ActionResult {
                            output: format!("Cancelled agent: {}", agent_id),
                            done: false,
                        }),
                        Err(e) => Ok(ActionResult {
                            output: format!("Failed to cancel: {}", e),
                            done: false,
                        }),
                    }
                }
            }
        }
    }
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
        let history: Vec<(String, String)> = messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();
        let input_chars: usize = history.iter().map(|(_, c)| c.len()).sum();

        let provider = self
            .provider
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No LLM provider configured"))?;

        let mut sgr_msgs = backend::to_sgr_messages(&history);
        sgr_msgs.insert(0, sgr_agent::Message::system(SGR_SYSTEM_PROMPT));
        let step = provider.call_flexible(&sgr_msgs).await?;

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
        let mcp_names: Vec<&str> = self
            .mcp
            .as_ref()
            .map(|m| m.server_names())
            .unwrap_or_default();

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
            other => baml_agent::loop_detect::normalize_signature(&Self::action_signature(other)),
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
        let history: Vec<(String, String)> = messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();
        let input_chars: usize = history.iter().map(|(_, c)| c.len()).sum();
        let provider = self.provider.clone();
        async move {
            let provider = provider.ok_or_else(|| anyhow::anyhow!("No LLM provider configured"))?;

            let mut sgr_msgs = backend::to_sgr_messages(&history);
            sgr_msgs.insert(0, sgr_agent::Message::system(SGR_SYSTEM_PROMPT));
            let step = provider.call_flexible(&sgr_msgs).await?;
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
            let mcp_names: Vec<&str> = self
                .mcp
                .as_ref()
                .map(|m| m.server_names())
                .unwrap_or_default();

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
        use baml_agent::LoopStatus;
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

        let ctx = baml_agent::AgentContext::load(dir.to_str().unwrap());
        assert_eq!(ctx.parts.len(), 2);
        let msg = ctx.to_system_message().unwrap();
        assert!(msg.contains("Be direct"));
        assert!(msg.contains("test-agent"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
