use anyhow::Result;
use crate::baml_client::{self, types};
use crate::tools::{
    read_file, write_file, run_command, FuzzySearcher, git_status, git_diff, git_add, git_commit,
    build_skills_context,
    mcp::McpManager,
};
use baml_agent::{AgentMessage, MessageRole, Session, LoopDetector};
use std::path::Path;

pub enum AgentEvent {
    Message(String),
    OpenEditor(String, Option<i64>),
}

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

const SESSION_DIR: &str = ".rust-code";
const MAX_HISTORY: usize = 60;

impl Agent {
    pub fn new() -> Self {
        let mut session = Session::new(SESSION_DIR, MAX_HISTORY);

        // Inject installed skills context as system message
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
        if let Some(resumed) = Session::resume_last(SESSION_DIR, MAX_HISTORY) {
            self.session = resumed;
        }
        Ok(())
    }

    pub fn load_session_file(&mut self, path: &Path) -> Result<()> {
        self.session = Session::resume(path, SESSION_DIR, MAX_HISTORY);
        Ok(())
    }

    pub fn history(&self) -> Vec<&types::Message> {
        self.session.messages().iter().map(|m| &m.0).collect()
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

    pub async fn step(&mut self) -> Result<types::NextStep> {
        self.session.trim();
        let history = self.baml_history();
        let response = baml_client::async_client::B.GetNextStep.call(&history).await?;
        Ok(response)
    }

    pub fn step_stream(&mut self) -> Result<baml::AsyncStreamingCall<baml_client::stream_types::NextStep, types::NextStep>> {
        self.session.trim();
        let history = self.baml_history();
        let stream = baml_client::async_client::B.GetNextStep.stream(&history)?;
        Ok(stream)
    }

    pub async fn execute_action(&mut self, action: &types::Union14AskUserToolOrBashBgToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrMcpToolCallOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool) -> Result<AgentEvent> {
        use types::Union14AskUserToolOrBashBgToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrMcpToolCallOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::*;
        match action {
            ReadFileTool(cmd) => {
                let content = read_file(&cmd.path, cmd.offset.map(|o| o as usize), cmd.limit.map(|l| l as usize)).await?;
                Ok(AgentEvent::Message(format!("File contents of {}:\n{}", cmd.path, content)))
            }
            WriteFileTool(cmd) => {
                write_file(&cmd.path, &cmd.content).await?;
                Ok(AgentEvent::Message(format!("Successfully wrote to {}", cmd.path)))
            }
            EditFileTool(cmd) => {
                crate::tools::fs::edit_file(&cmd.path, &cmd.old_string, &cmd.new_string).await?;
                Ok(AgentEvent::Message(format!("Successfully edited {}", cmd.path)))
            }
            BashCommandTool(cmd) => {
                let output = run_command(&cmd.command).await?;
                Ok(AgentEvent::Message(format!("Command output:\n{}", output)))
            }
            BashBgTool(cmd) => {
                let output = crate::tools::run_command_bg(&cmd.name, &cmd.command).await?;
                Ok(AgentEvent::Message(format!("[BG] {}", output)))
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

                Ok(AgentEvent::Message(result))
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
                        Ok(AgentEvent::Message(result))
                    }
                    None => Ok(AgentEvent::Message("Not in a git repository".to_string())),
                }
            }
            GitDiffTool(cmd) => {
                let diff = git_diff(cmd.path.as_deref(), cmd.cached.unwrap_or(false))?;
                if diff.is_empty() {
                    Ok(AgentEvent::Message("No changes to show".to_string()))
                } else {
                    Ok(AgentEvent::Message(format!("Git Diff:\n{}", diff)))
                }
            }
            GitAddTool(cmd) => {
                git_add(&cmd.paths)?;
                Ok(AgentEvent::Message(format!("Added {} files to staging", cmd.paths.len())))
            }
            GitCommitTool(cmd) => {
                git_commit(&cmd.message)?;
                Ok(AgentEvent::Message(format!("Committed: {}", cmd.message)))
            }
            OpenEditorTool(cmd) => {
                Ok(AgentEvent::OpenEditor(cmd.path.clone(), cmd.line))
            }
            FinishTaskTool(cmd) => {
                Ok(AgentEvent::Message(format!("Task finished: {}", cmd.summary)))
            }
            AskUserTool(cmd) => {
                Ok(AgentEvent::Message(format!("Question for user: {}", cmd.question)))
            }
            McpToolCall(cmd) => {
                let Some(mcp) = &self.mcp else {
                    return Ok(AgentEvent::Message("MCP not initialized. No .mcp.json found.".to_string()));
                };
                let args = cmd.arguments.as_ref().and_then(|json_str| {
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json_str).ok()
                });
                match mcp.call_tool(&cmd.server, &cmd.tool, args).await {
                    Ok(result) => {
                        let output = crate::tools::mcp::format_tool_result(&result);
                        Ok(AgentEvent::Message(format!("MCP [{}] {}:\n{}", cmd.server, cmd.tool, output)))
                    }
                    Err(e) => {
                        Ok(AgentEvent::Message(format!("MCP Error [{}] {}: {}", cmd.server, cmd.tool, e)))
                    }
                }
            }
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
}
