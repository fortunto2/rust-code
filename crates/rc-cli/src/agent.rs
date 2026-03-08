use crate::baml_client::{self, types};
use crate::tools::{
    FuzzySearcher, build_skills_context, cost, create_checkpoint, git_add, git_commit, git_diff,
    git_status, is_mutating_action, mcp::McpManager, read_file, run_command, write_file,
};
use anyhow::Result;
use baml_agent::{
    ActionKind, ActionResult, AgentMessage, HintContext, Intent, LoopDetector, MessageRole,
    Session, SgrAgent, SgrAgentStream, StepDecision, collect_hints,
};
use std::path::Path;

/// Shorthand for the 18-variant BAML action union.
pub use types::Union18AskUserToolOrBashBgToolOrBashCommandToolOrDependenciesToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrMcpToolCallOrMemoryToolOrOpenEditorToolOrProjectMapToolOrReadFileToolOrSearchCodeToolOrTaskToolOrWriteFileTool as Action;

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

/// Wrapper around BAML's Message that implements baml-agent traits.
#[derive(Clone)]
pub struct Msg(pub types::Message);

impl AgentMessage for Msg {
    type Role = Role;
    fn new(role: Role, content: String) -> Self {
        Self(types::Message {
            role: role.0,
            content,
        })
    }
    fn role(&self) -> &Role {
        // Safety: Role is repr(String), same layout
        unsafe { &*((&self.0.role) as *const String as *const Role) }
    }
    fn content(&self) -> &str {
        &self.0.content
    }
}

pub struct Agent {
    session: Session<Msg>,
    mcp: Option<McpManager>,
    step_count: usize,
    last_input_chars: usize,
    /// Override BAML client name (e.g. "OllamaDefault" for --local)
    client_override: Option<String>,
    /// Current user intent for action filtering.
    pub intent: Intent,
    /// Pluggable hint sources.
    hint_sources: Vec<Box<dyn baml_agent::HintSource>>,
    /// Persistent CWD for bash commands (tracks `cd` across steps).
    /// Interior mutability: execute() takes &self but needs to update CWD.
    cwd: std::sync::Mutex<std::path::PathBuf>,
}

const AGENT_HOME: &str = ".rust-code";
const MAX_HISTORY: usize = 60;

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
            client_override: None,
            intent: Intent::Auto,
            hint_sources: baml_agent::default_sources_with_tasks(Path::new(".")),
            cwd: std::sync::Mutex::new(
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            ),
        }
    }

    /// Create a new LoopDetector (used by callers in app.rs/main.rs).
    pub fn new_loop_detector() -> LoopDetector {
        LoopDetector::new(6)
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

    pub fn history(&self) -> Vec<&types::Message> {
        self.session.messages().iter().map(|m| &m.0).collect()
    }

    /// Get mutable reference to session (for run_loop / TUI).
    pub fn session_mut(&mut self) -> &mut Session<Msg> {
        &mut self.session
    }

    /// Get raw BAML messages for the LLM call.
    ///
    /// Injects ephemeral project map after system messages (not stored in session).
    /// - First call: full repomap (all top files with symbols)
    /// - Subsequent calls: compact summary + detailed symbols for changed files only
    fn baml_history(&mut self) -> Vec<types::Message> {
        self.step_count += 1;

        let msgs: Vec<_> = self
            .session
            .messages()
            .iter()
            .map(|m| m.0.clone())
            .collect();

        // Find where system messages end to insert repomap there
        let insert_at = msgs
            .iter()
            .rposition(|m| m.role == "system")
            .map(|i| i + 1)
            .unwrap_or(0);

        let root = Path::new(".");
        let map_content = if self.step_count <= 1 {
            // First call: full repomap so model understands the project
            let repomap = solograph::generate_repomap(root);
            format!(
                "## Project Map (full, auto-generated)\n```\n{}\n```",
                repomap
            )
        } else {
            // Subsequent calls: compact summary + changed files detail
            let changed = git_changed_files();
            let context_map = solograph::generate_context_map(root, &changed);
            format!(
                "## Project Map (compact, {} changed files)\n```\n{}\n```",
                changed.len(),
                context_map
            )
        };

        let repomap_msg = types::Message {
            role: "system".into(),
            content: map_content,
        };

        let mut result = Vec::with_capacity(msgs.len() + 1);
        result.extend_from_slice(&msgs[..insert_at]);
        result.push(repomap_msg);
        result.extend_from_slice(&msgs[insert_at..]);
        result
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.session.push(Role::user(), content.into());
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        self.session.push(Role::assistant(), content.into());
    }

    /// TUI-only: get streaming BAML call (used by app.rs manual loop).
    pub fn step_stream(
        &mut self,
    ) -> Result<baml::AsyncStreamingCall<baml_client::stream_types::NextStep, types::NextStep>>
    {
        self.session.trim();
        let history = self.baml_history();

        // Record input size for cost tracking
        let input_chars: usize = history.iter().map(|m| m.content.len()).sum();
        self.last_input_chars = input_chars;

        let stream = if let Some(ref client) = self.client_override {
            baml_client::async_client::B
                .GetNextStep
                .with_client(client)
                .stream(&history)?
        } else {
            baml_client::async_client::B.GetNextStep.stream(&history)?
        };
        Ok(stream)
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

    /// Set BAML client override (e.g. "OllamaDefault" for local mode).
    pub fn set_client(&mut self, client_name: impl Into<String>) {
        self.client_override = Some(client_name.into());
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
        use Action::*;
        match action {
            ReadFileTool(cmd) => {
                let content = read_file(
                    &cmd.path,
                    cmd.offset.map(|o| o as usize),
                    cmd.limit.map(|l| l as usize),
                )
                .await?;
                Ok(ActionResult {
                    output: format!("File contents of {}:\n{}", cmd.path, content),
                    done: false,
                })
            }
            WriteFileTool(cmd) => {
                let is_new = !std::path::Path::new(&cmd.path).exists();
                write_file(&cmd.path, &cmd.content).await?;
                let label = if is_new { "Created" } else { "Wrote" };
                let lines = cmd.content.lines().count();
                Ok(ActionResult {
                    output: format!("{} {} ({} lines)", label, cmd.path, lines),
                    done: false,
                })
            }
            EditFileTool(cmd) => {
                crate::tools::edit_file(&cmd.path, &cmd.old_string, &cmd.new_string).await?;
                // Show inline diff
                let old_lines: Vec<&str> = cmd.old_string.lines().collect();
                let new_lines: Vec<&str> = cmd.new_string.lines().collect();
                let mut diff = format!(
                    "Edited {} ({}→{} lines)\n",
                    cmd.path,
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
            BashCommandTool(cmd) => {
                let timeout_ms = cmd.timeout.map(|t| (t as u64).min(600_000));
                let current_cwd = self.cwd.lock().unwrap().clone();
                let result =
                    crate::tools::run_command_in(&cmd.command, &current_cwd, timeout_ms).await;
                *self.cwd.lock().unwrap() = result.cwd;
                let exit_info = if result.exit_code == 0 {
                    String::new()
                } else {
                    format!("\n[exit code: {}]", result.exit_code)
                };
                Ok(ActionResult {
                    output: format!("Command output:\n{}{}", result.output, exit_info),
                    done: false,
                })
            }
            BashBgTool(cmd) => {
                let output = crate::tools::run_command_bg(&cmd.name, &cmd.command).await?;
                Ok(ActionResult {
                    output: format!("[BG] {}", output),
                    done: false,
                })
            }
            SearchCodeTool(cmd) => {
                let mut result = String::new();

                if let Ok(files) = FuzzySearcher::get_all_files().await {
                    let mut searcher = FuzzySearcher::new();
                    let matches = searcher.fuzzy_match_files(&cmd.query, &files);
                    if !matches.is_empty() {
                        result.push_str(&format!("File path matches for '{}':\n", cmd.query));
                        for (score, path) in matches.iter().take(5) {
                            if *score > 50 {
                                result.push_str(&format!("- {}\n", path));
                            }
                        }
                        result.push('\n');
                    }
                }

                result.push_str(&format!("Content search results for '{}':\n", cmd.query));
                let safe_query = cmd.query.replace("'", "'\\''");
                let search_cmd = format!("rg -n '{}' . || grep -rn '{}' .", safe_query, safe_query);
                match run_command(&search_cmd).await {
                    Ok(output) => {
                        if output.trim().is_empty() {
                            result.push_str("No content matches found.");
                        } else {
                            let lines: Vec<&str> = output.lines().collect();
                            if lines.len() > 100 {
                                result.push_str(&lines[..100].join("\n"));
                                result.push_str(&format!(
                                    "\n...[Truncated {} more lines]...",
                                    lines.len() - 100
                                ));
                            } else {
                                result.push_str(&output);
                            }
                        }
                    }
                    Err(_) => {
                        result.push_str("No content matches found or search tool failed.");
                    }
                }

                Ok(ActionResult {
                    output: result,
                    done: false,
                })
            }
            GitStatusTool(_cmd) => match git_status()? {
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
            GitDiffTool(cmd) => {
                let diff = git_diff(cmd.path.as_deref(), cmd.cached.unwrap_or(false))?;
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
            GitAddTool(cmd) => {
                git_add(&cmd.paths)?;
                Ok(ActionResult {
                    output: format!("Added {} files to staging", cmd.paths.len()),
                    done: false,
                })
            }
            GitCommitTool(cmd) => {
                git_commit(&cmd.message)?;
                Ok(ActionResult {
                    output: format!("Committed: {}", cmd.message),
                    done: false,
                })
            }
            OpenEditorTool(cmd) => Ok(ActionResult {
                output: format!("Opened {} in editor", cmd.path),
                done: false,
            }),
            FinishTaskTool(cmd) => Ok(ActionResult {
                output: format!("Task finished: {}", cmd.summary),
                done: true,
            }),
            AskUserTool(cmd) => Ok(ActionResult {
                output: format!("Question for user: {}", cmd.question),
                done: true,
            }),
            MemoryTool(cmd) => {
                let memory_path = Path::new(AGENT_HOME).join("MEMORY.jsonl");
                let op = baml_agent::norm(&format!("{:?}", cmd.operation));
                let category = baml_agent::norm(&format!("{:?}", cmd.category));
                let confidence = baml_agent::norm(&format!("{:?}", cmd.confidence));

                match op.as_str() {
                    "save" => {
                        let entry = serde_json::json!({
                            "category": category,
                            "section": cmd.section,
                            "content": cmd.content,
                            "context": cmd.context,
                            "confidence": confidence,
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
                            output: format!(
                                "Memory saved: [{}] {} ({})",
                                category, cmd.section, confidence
                            ),
                            done: false,
                        })
                    }
                    "forget" => {
                        if memory_path.exists() {
                            let content = std::fs::read_to_string(&memory_path).unwrap_or_default();
                            let filtered: Vec<&str> = content
                                .lines()
                                .filter(|line| {
                                    serde_json::from_str::<serde_json::Value>(line)
                                        .map(|v| v["section"].as_str() != Some(&cmd.section))
                                        .unwrap_or(true)
                                })
                                .collect();
                            let removed = content.lines().count() - filtered.len();
                            std::fs::write(&memory_path, filtered.join("\n") + "\n")
                                .map_err(|e| anyhow::anyhow!("Memory write: {}", e))?;
                            Ok(ActionResult {
                                output: format!(
                                    "Memory: forgot {} entries from '{}'",
                                    removed, cmd.section
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
            McpToolCall(cmd) => {
                let Some(mcp) = &self.mcp else {
                    return Ok(ActionResult {
                        output: "MCP not initialized. No .mcp.json found.".into(),
                        done: false,
                    });
                };
                let args = cmd.arguments.as_ref().and_then(|json_str| {
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json_str)
                        .ok()
                });
                match mcp.call_tool(&cmd.server, &cmd.tool, args).await {
                    Ok(result) => {
                        let output = crate::tools::mcp::format_tool_result(&result);
                        Ok(ActionResult {
                            output: format!("MCP [{}] {}:\n{}", cmd.server, cmd.tool, output),
                            done: false,
                        })
                    }
                    Err(e) => Ok(ActionResult {
                        output: format!("MCP Error [{}] {}: {}", cmd.server, cmd.tool, e),
                        done: false,
                    }),
                }
            }
            ProjectMapTool(cmd) => {
                let dir = cmd.path.as_deref().unwrap_or(".");
                let map = solograph::generate_repomap(Path::new(dir));
                Ok(ActionResult {
                    output: map,
                    done: false,
                })
            }
            TaskTool(cmd) => {
                let project_root = Path::new(".");
                let op = baml_agent::norm(&format!("{:?}", cmd.operation));
                match op.as_str() {
                    "create" => {
                        let title = cmd.title.as_deref().unwrap_or("Untitled");
                        let priority = cmd
                            .priority
                            .as_ref()
                            .and_then(|p| {
                                baml_agent::Priority::parse(&baml_agent::norm(&format!("{:?}", p)))
                            })
                            .unwrap_or(baml_agent::Priority::Medium);
                        let mut task = baml_agent::create_task(project_root, title, priority);
                        if let Some(notes) = &cmd.notes {
                            task.body = notes.clone();
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
                                output: "No tasks found. Use TaskTool(operation='create') to create one.".into(),
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
                        let Some(id) = cmd.task_id else {
                            return Ok(ActionResult {
                                output: "Error: task_id required for update".into(),
                                done: false,
                            });
                        };
                        let id = id as u16;
                        if let Some(status_val) = &cmd.status {
                            let status_str = baml_agent::norm(&format!("{:?}", status_val));
                            if let Some(status) = baml_agent::TaskStatus::parse(&status_str) {
                                baml_agent::update_status(project_root, id, status);
                            }
                        }
                        if let Some(notes) = &cmd.notes {
                            baml_agent::append_notes(project_root, id, notes);
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
                        let Some(id) = cmd.task_id else {
                            return Ok(ActionResult {
                                output: "Error: task_id required for done".into(),
                                done: false,
                            });
                        };
                        match baml_agent::update_status(
                            project_root,
                            id as u16,
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
            DependenciesTool(cmd) => {
                let path = if let Some(p) = &cmd.path {
                    std::path::PathBuf::from(p)
                } else {
                    // Auto-detect manifest in current dir
                    ["Cargo.toml", "package.json", "pyproject.toml"]
                        .iter()
                        .map(std::path::PathBuf::from)
                        .find(|p| p.exists())
                        .unwrap_or_else(|| std::path::PathBuf::from("Cargo.toml"))
                };
                let deps = solograph::parse_deps(&path);
                if deps.is_empty() {
                    Ok(ActionResult {
                        output: format!("No dependencies found in {}", path.display()),
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
                        output: format!("Dependencies from {}:\n{}", path.display(), output),
                        done: false,
                    })
                }
            }
        }
    }
}

/// SgrAgent implementation — used by run_loop_stream in headless mode.
///
/// `execute` delegates to `execute_action` which takes `&self` (no mutation).
/// `decide`/`decide_stream` call BAML directly from the passed-in messages.
impl SgrAgent for Agent {
    type Action = Action;
    type Msg = Msg;
    type Error = anyhow::Error;

    async fn decide(&self, messages: &[Msg]) -> Result<StepDecision<Action>> {
        let history: Vec<types::Message> = messages.iter().map(|m| m.0.clone()).collect();
        let input_chars: usize = history.iter().map(|m| m.content.len()).sum();

        let step = if let Some(ref client) = self.client_override {
            baml_client::async_client::B
                .GetNextStep
                .with_client(client)
                .call(&history)
                .await?
        } else {
            baml_client::async_client::B
                .GetNextStep
                .call(&history)
                .await?
        };

        // Track cost: estimate output from response fields
        let output_chars = step.situation.len()
            + step.task.iter().map(|t| t.len()).sum::<usize>()
            + format!("{:?}", step.actions).len();
        cost::record_step(input_chars, output_chars);

        let done = step
            .actions
            .iter()
            .any(|a| matches!(a, Action::FinishTaskTool(_) | Action::AskUserTool(_)));

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

    fn action_signature(action: &Action) -> String {
        use Action::*;
        match action {
            ReadFileTool(c) => format!("read:{}", c.path),
            WriteFileTool(c) => format!("write:{}", c.path),
            EditFileTool(c) => format!("edit:{}", c.path),
            BashCommandTool(c) => format!("bash:{}", c.command),
            BashBgTool(c) => format!("bg:{}", c.name),
            SearchCodeTool(c) => format!("search:{}", c.query),
            GitStatusTool(_) => "git_status".into(),
            GitDiffTool(c) => format!("diff:{:?}", c.path),
            GitAddTool(c) => format!("add:{:?}", c.paths),
            GitCommitTool(c) => format!("commit:{}", c.message),
            OpenEditorTool(c) => format!("open:{}", c.path),
            AskUserTool(c) => format!("ask:{}", c.question),
            FinishTaskTool(c) => format!("finish:{}", c.summary),
            MemoryTool(c) => format!("memory:{:?}:{}", c.operation, c.section),
            McpToolCall(c) => format!("mcp:{}:{}", c.server, c.tool),
            ProjectMapTool(c) => format!("project_map:{:?}", c.path),
            DependenciesTool(c) => format!("deps:{:?}", c.path),
            TaskTool(c) => format!("task:{:?}:{:?}", c.operation, c.task_id),
        }
    }
}

/// Classify action into coarse ActionKind for intent guard.
pub fn action_kind(action: &Action) -> ActionKind {
    use Action::*;
    match action {
        ReadFileTool(_) | SearchCodeTool(_) | GitStatusTool(_) | GitDiffTool(_)
        | ProjectMapTool(_) | DependenciesTool(_) => ActionKind::Read,
        WriteFileTool(_) | EditFileTool(_) | OpenEditorTool(_) => ActionKind::Write,
        BashCommandTool(_) | BashBgTool(_) => ActionKind::Execute,
        GitAddTool(_) | GitCommitTool(_) => ActionKind::GitMutate,
        AskUserTool(_) | FinishTaskTool(_) | MemoryTool(_) | TaskTool(_) => ActionKind::Plan,
        McpToolCall(_) => ActionKind::External,
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
        let history: Vec<types::Message> = messages.iter().map(|m| m.0.clone()).collect();
        let input_chars: usize = history.iter().map(|m| m.content.len()).sum();
        let client_override = self.client_override.clone();
        async move {
            let mut stream = if let Some(ref client) = client_override {
                baml_client::async_client::B
                    .GetNextStep
                    .with_client(client)
                    .stream(&history)?
            } else {
                baml_client::async_client::B.GetNextStep.stream(&history)?
            };
            let mut last_analysis_len = 0;

            while let Some(partial) = stream.next().await {
                match partial {
                    Ok(partial_step) => {
                        if let Some(ref analysis) = partial_step.situation {
                            if analysis.len() > last_analysis_len {
                                on_token(&analysis[last_analysis_len..]);
                                last_analysis_len = analysis.len();
                            }
                        }
                    }
                    Err(_) => {}
                }
            }

            let step = stream.get_final_response().await?;

            // Track cost
            let output_chars = step.situation.len()
                + step.task.iter().map(|t| t.len()).sum::<usize>()
                + format!("{:?}", step.actions).len();
            cost::record_step(input_chars, output_chars);
            let done = step
                .actions
                .iter()
                .any(|a| matches!(a, Action::FinishTaskTool(_) | Action::AskUserTool(_)));

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
        assert_eq!(ld.check("a"), LoopStatus::Warning(3));
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
