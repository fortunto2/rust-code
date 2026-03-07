use anyhow::Result;
use crate::baml_client::{self, types};
use crate::tools::{
    read_file, write_file, run_command, FuzzySearcher, git_status, git_diff, git_add, git_commit,
    build_skills_context,
    mcp::McpManager,
};
use baml_agent::{AgentMessage, MessageRole, Session, LoopDetector, SgrAgent, SgrAgentStream, StepDecision, ActionResult};
use std::path::Path;

/// Shorthand for the 15-variant BAML action union.
pub use types::Union15AskUserToolOrBashBgToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrMcpToolCallOrMemoryToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool as Action;

// Implement baml-agent traits for BAML's generated Message type

#[derive(Clone, PartialEq)]
pub struct Role(String);

impl MessageRole for Role {
    fn system() -> Self { Self("system".into()) }
    fn user() -> Self { Self("user".into()) }
    fn assistant() -> Self { Self("assistant".into()) }
    fn tool() -> Self { Self("tool".into()) }
    fn as_str(&self) -> &str { &self.0 }
    fn from_str(s: &str) -> Option<Self> {
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
        Self(types::Message { role: role.0, content })
    }
    fn role(&self) -> &Role {
        // Safety: Role is repr(String), same layout
        unsafe { &*((&self.0.role) as *const String as *const Role) }
    }
    fn content(&self) -> &str { &self.0.content }
}

pub struct Agent {
    session: Session<Msg>,
    mcp: Option<McpManager>,
}

const AGENT_HOME: &str = ".rust-code";
const MAX_HISTORY: usize = 60;

impl Agent {
    pub fn new() -> Self {
        let mut session = Session::new(AGENT_HOME, MAX_HISTORY);

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

        Self { session, mcp: None }
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

        tracing::info!("MCP ready: {} servers, {} tools", manager.server_count(), manager.tool_count());
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
    fn baml_history(&self) -> Vec<types::Message> {
        self.session.messages().iter().map(|m| m.0.clone()).collect()
    }

    pub fn add_user_message(&mut self, content: impl Into<String>) {
        self.session.push(Role::user(), content.into());
    }

    pub fn add_assistant_message(&mut self, content: impl Into<String>) {
        self.session.push(Role::assistant(), content.into());
    }

    /// TUI-only: get streaming BAML call (used by app.rs manual loop).
    pub fn step_stream(&mut self) -> Result<baml::AsyncStreamingCall<baml_client::stream_types::NextStep, types::NextStep>> {
        self.session.trim();
        let history = self.baml_history();
        let stream = baml_client::async_client::B.GetNextStep.stream(&history)?;
        Ok(stream)
    }

    /// Execute a single action. Returns tool output + done flag.
    pub async fn execute_action(&self, action: &Action) -> Result<ActionResult> {
        use Action::*;
        match action {
            ReadFileTool(cmd) => {
                let content = read_file(&cmd.path, cmd.offset.map(|o| o as usize), cmd.limit.map(|l| l as usize)).await?;
                Ok(ActionResult {
                    output: format!("File contents of {}:\n{}", cmd.path, content),
                    done: false,
                })
            }
            WriteFileTool(cmd) => {
                write_file(&cmd.path, &cmd.content).await?;
                Ok(ActionResult {
                    output: format!("Successfully wrote to {}", cmd.path),
                    done: false,
                })
            }
            EditFileTool(cmd) => {
                crate::tools::fs::edit_file(&cmd.path, &cmd.old_string, &cmd.new_string).await?;
                Ok(ActionResult {
                    output: format!("Successfully edited {}", cmd.path),
                    done: false,
                })
            }
            BashCommandTool(cmd) => {
                let output = run_command(&cmd.command).await?;
                Ok(ActionResult {
                    output: format!("Command output:\n{}", output),
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
                                result.push_str(&format!("\n...[Truncated {} more lines]...", lines.len() - 100));
                            } else {
                                result.push_str(&output);
                            }
                        }
                    }
                    Err(_) => {
                        result.push_str("No content matches found or search tool failed.");
                    }
                }

                Ok(ActionResult { output: result, done: false })
            }
            GitStatusTool(_cmd) => {
                match git_status()? {
                    Some(status) => {
                        let mut result = format!("Git Status:\nBranch: {}\nDirty: {}\n", status.branch, status.dirty);
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
                        Ok(ActionResult { output: result, done: false })
                    }
                    None => Ok(ActionResult { output: "Not in a git repository".into(), done: false }),
                }
            }
            GitDiffTool(cmd) => {
                let diff = git_diff(cmd.path.as_deref(), cmd.cached.unwrap_or(false))?;
                let output = if diff.is_empty() {
                    "No changes to show".into()
                } else {
                    format!("Git Diff:\n{}", diff)
                };
                Ok(ActionResult { output, done: false })
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
            OpenEditorTool(cmd) => {
                Ok(ActionResult {
                    output: format!("Opened {} in editor", cmd.path),
                    done: false,
                })
            }
            FinishTaskTool(cmd) => {
                Ok(ActionResult {
                    output: format!("Task finished: {}", cmd.summary),
                    done: true,
                })
            }
            AskUserTool(cmd) => {
                Ok(ActionResult {
                    output: format!("Question for user: {}", cmd.question),
                    done: true,
                })
            }
            MemoryTool(cmd) => {
                let memory_path = Path::new(AGENT_HOME).join("MEMORY.md");
                let op = baml_agent::norm(&format!("{:?}", cmd.operation));
                let result = match op.as_str() {
                    "append" => {
                        let mut existing = std::fs::read_to_string(&memory_path).unwrap_or_default();
                        let section_header = format!("## {}", cmd.section);
                        if existing.contains(&section_header) {
                            // Append under existing section (before next ## or EOF)
                            if let Some(pos) = existing.find(&section_header) {
                                let after_header = pos + section_header.len();
                                let next_section = existing[after_header..].find("\n## ").map(|p| after_header + p);
                                let insert_at = next_section.unwrap_or(existing.len());
                                existing.insert_str(insert_at, &format!("\n{}\n", cmd.content));
                            }
                        } else {
                            // New section
                            if !existing.is_empty() && !existing.ends_with('\n') {
                                existing.push('\n');
                            }
                            existing.push_str(&format!("\n{}\n{}\n", section_header, cmd.content));
                        }
                        std::fs::write(&memory_path, &existing)
                    }
                    "replace" => {
                        let mut existing = std::fs::read_to_string(&memory_path).unwrap_or_default();
                        let section_header = format!("## {}", cmd.section);
                        if let Some(pos) = existing.find(&section_header) {
                            let after_header = pos + section_header.len();
                            let next_section = existing[after_header..].find("\n## ").map(|p| after_header + p + 1);
                            let end = next_section.unwrap_or(existing.len());
                            existing.replace_range(pos..end, &format!("{}\n{}\n", section_header, cmd.content));
                        } else {
                            if !existing.is_empty() && !existing.ends_with('\n') {
                                existing.push('\n');
                            }
                            existing.push_str(&format!("\n{}\n{}\n", section_header, cmd.content));
                        }
                        std::fs::write(&memory_path, &existing)
                    }
                    _ => {
                        return Ok(ActionResult {
                            output: format!("Unknown memory operation: {}", op),
                            done: false,
                        });
                    }
                };
                match result {
                    Ok(_) => Ok(ActionResult {
                        output: format!("Memory updated: [{}] {}", op, cmd.section),
                        done: false,
                    }),
                    Err(e) => Ok(ActionResult {
                        output: format!("Memory write error: {}", e),
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
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json_str).ok()
                });
                match mcp.call_tool(&cmd.server, &cmd.tool, args).await {
                    Ok(result) => {
                        let output = crate::tools::mcp::format_tool_result(&result);
                        Ok(ActionResult {
                            output: format!("MCP [{}] {}:\n{}", cmd.server, cmd.tool, output),
                            done: false,
                        })
                    }
                    Err(e) => {
                        Ok(ActionResult {
                            output: format!("MCP Error [{}] {}: {}", cmd.server, cmd.tool, e),
                            done: false,
                        })
                    }
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
        let step = baml_client::async_client::B.GetNextStep.call(&history).await?;

        let done = step.actions.iter().any(|a| {
            matches!(a, Action::FinishTaskTool(_) | Action::AskUserTool(_))
        });

        Ok(StepDecision {
            situation: step.situation,
            task: step.task,
            completed: done,
            actions: step.actions,
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
        }
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
        async move {
            let mut stream = baml_client::async_client::B.GetNextStep.stream(&history)?;
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
            let done = step.actions.iter().any(|a| {
                matches!(a, Action::FinishTaskTool(_) | Action::AskUserTool(_))
            });

            Ok(StepDecision {
                situation: step.situation,
                task: step.task,
                completed: done,
                actions: step.actions,
            })
        }
    }
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
