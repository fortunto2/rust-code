//! Delegate tasks to external CLI agents (claude, gemini, codex, opencode, rust-code)
//! running as full autonomous agents in tmux background.
//!
//! Two modes:
//! - **Free-text**: `delegate_task {agent: "claude", task: "fix the bug"}`
//! - **Task file**: `delegate_task {agent: "claude", task_path: ".tasks/005-fix-bug.md"}`
//!   Agent reads the task file, executes, updates status to done, writes results in body.
//!
//! Agents inherit CLAUDE.md / project context automatically when running in the project dir.

use super::bash::{list_bg_windows, read_tmux_log, run_command_bg};
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Supported delegate agents.
#[derive(Debug, Clone, Copy)]
pub enum DelegateAgent {
    Claude,
    Gemini,
    Codex,
    OpenCode,
    RustCode,
}

impl DelegateAgent {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "claude" | "claude-code" => Some(Self::Claude),
            "gemini" | "gemini-cli" => Some(Self::Gemini),
            "codex" | "codex-cli" => Some(Self::Codex),
            "opencode" | "open-code" | "oc" => Some(Self::OpenCode),
            "rust-code" | "rustcode" | "rc" => Some(Self::RustCode),
            _ => None,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::RustCode => "rust-code",
        }
    }

    /// Check if the agent CLI is installed and accessible.
    pub async fn check_available(&self) -> Result<(), String> {
        let bin = match self {
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
            Self::RustCode => "rust-code",
        };

        let output = tokio::process::Command::new(bin)
            .arg("--version")
            .output()
            .await
            .map_err(|_| format!("{bin} not found. Is it installed?"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("{bin} check failed: {}", stderr.trim()));
        }
        Ok(())
    }

    /// Build the shell command for full autonomous headless mode.
    fn build_command(&self, task: &str, cwd: &Path) -> String {
        let escaped_task = task.replace('\'', "'\\''");
        let cd = format!("cd '{}'", cwd.display());

        match self {
            Self::Claude => {
                format!(
                    "{cd} && CLAUDECODE='' claude -p '{escaped_task}' \
                     --output-format json --dangerously-skip-permissions --verbose"
                )
            }
            Self::Gemini => {
                format!("{cd} && gemini -p '{escaped_task}' --sandbox -y")
            }
            Self::Codex => {
                format!(
                    "{cd} && codex exec '{escaped_task}' \
                     --dangerously-bypass-approvals-and-sandbox"
                )
            }
            Self::OpenCode => {
                format!("{cd} && opencode run '{escaped_task}' --format json")
            }
            Self::RustCode => {
                format!("{cd} && rust-code -p '{escaped_task}' --loop 5")
            }
        }
    }
}

/// Build a prompt from a task file path.
/// Wraps the original task with instructions to update the task file on completion.
fn build_task_prompt(task_path: &str, extra_task: Option<&str>) -> String {
    let mut prompt = format!(
        "Your assignment is in the file: {task_path}\n\
         Read it first to understand the task.\n\n"
    );

    if let Some(extra) = extra_task {
        prompt.push_str(&format!("Additional instructions: {extra}\n\n"));
    }

    prompt.push_str(
        "When you complete the task:\n\
         1. Update the task file — change `status: todo` (or `in_progress`) to `status: done`\n\
         2. Add a `## Results` section at the end of the task body with a summary of what you did\n\
         3. If you made code changes, commit them\n\n\
         Read CLAUDE.md or project docs first for conventions. Start working now.",
    );

    prompt
}

/// Handle for a running delegate.
pub struct DelegateHandle {
    pub agent: DelegateAgent,
    pub task: String,
    pub task_path: Option<String>,
    pub tmux_window: String,
    pub started_at: Instant,
}

/// Status of a delegate.
pub enum DelegateStatus {
    Running,
    Done,
    Unknown,
}

impl std::fmt::Display for DelegateStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Done => write!(f, "done"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

/// Manages delegate lifecycle — spawn, status, result, cancel.
pub struct DelegateManager {
    delegates: HashMap<String, DelegateHandle>,
    counter: u64,
}

impl DelegateManager {
    pub fn new() -> Self {
        Self {
            delegates: HashMap::new(),
            counter: 0,
        }
    }

    /// Spawn a delegate agent in tmux background.
    ///
    /// - `task`: free-text task description (optional if task_path given)
    /// - `task_path`: path to .tasks/ file — agent reads it, executes, updates status
    pub async fn spawn(
        &mut self,
        agent: DelegateAgent,
        task: Option<&str>,
        task_path: Option<&str>,
        cwd: &Path,
    ) -> Result<String> {
        agent
            .check_available()
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Build the prompt
        let prompt = if let Some(tp) = task_path {
            // Task file mode: read file, execute, update status
            let abs_path = if Path::new(tp).is_absolute() {
                tp.to_string()
            } else {
                cwd.join(tp).display().to_string()
            };

            // Verify file exists
            if !Path::new(&abs_path).exists() {
                anyhow::bail!("Task file not found: {}", abs_path);
            }

            // Mark task as in_progress
            if let Ok(content) = std::fs::read_to_string(&abs_path) {
                let updated = content.replace("status: todo", "status: in_progress");
                if updated != content {
                    let _ = std::fs::write(&abs_path, &updated);
                }
            }

            build_task_prompt(tp, task)
        } else if let Some(t) = task {
            t.to_string()
        } else {
            anyhow::bail!("Either 'task' or 'task_path' must be provided");
        };

        self.counter += 1;
        let id = format!("del-{}", self.counter);
        let window_name = id.clone();

        let command = agent.build_command(&prompt, cwd);
        run_command_bg(&window_name, &command).await?;

        let display_task = task
            .map(|t| t.to_string())
            .or_else(|| task_path.map(|p| format!("[task: {}]", p)))
            .unwrap_or_default();

        self.delegates.insert(
            id.clone(),
            DelegateHandle {
                agent,
                task: display_task,
                task_path: task_path.map(String::from),
                tmux_window: window_name,
                started_at: Instant::now(),
            },
        );

        Ok(id)
    }

    /// Check status of a specific delegate.
    /// If task_path is set and the task file shows status: done, report as done.
    pub async fn status(&self, id: &str) -> Option<(DelegateStatus, std::time::Duration)> {
        let handle = self.delegates.get(id)?;
        let elapsed = handle.started_at.elapsed();

        // Check task file first — agent may have updated it
        if let Some(ref tp) = handle.task_path {
            if let Ok(content) = std::fs::read_to_string(tp) {
                if content.contains("status: done") {
                    return Some((DelegateStatus::Done, elapsed));
                }
            }
        }

        let status = self.check_window_status(&handle.tmux_window).await;
        Some((status, elapsed))
    }

    /// Status of all delegates.
    pub async fn status_all(&self) -> Vec<(String, String, DelegateStatus, std::time::Duration)> {
        let windows = list_bg_windows().await.unwrap_or_default();
        let window_done: HashMap<&str, bool> =
            windows.iter().map(|(n, d)| (n.as_str(), *d)).collect();

        self.delegates
            .iter()
            .map(|(id, handle)| {
                let elapsed = handle.started_at.elapsed();

                // Check task file first
                let task_done = handle.task_path.as_ref().is_some_and(|tp| {
                    std::fs::read_to_string(tp)
                        .map(|c| c.contains("status: done"))
                        .unwrap_or(false)
                });

                let status = if task_done {
                    DelegateStatus::Done
                } else {
                    match window_done.get(handle.tmux_window.as_str()) {
                        Some(true) => DelegateStatus::Done,
                        Some(false) => DelegateStatus::Running,
                        None => DelegateStatus::Unknown,
                    }
                };

                (
                    id.clone(),
                    handle.agent.display_name().to_string(),
                    status,
                    elapsed,
                )
            })
            .collect()
    }

    /// Get result from a delegate.
    /// If task_path is set, reads the task file (which should have ## Results).
    /// Otherwise falls back to tmux buffer.
    pub async fn result(&self, id: &str) -> Result<String> {
        let handle = self
            .delegates
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Delegate '{}' not found", id))?;

        // Prefer task file — has structured results
        if let Some(ref tp) = handle.task_path {
            if let Ok(content) = std::fs::read_to_string(tp) {
                if content.contains("## Results") || content.contains("status: done") {
                    return Ok(content);
                }
            }
        }

        // Fallback: read tmux buffer
        let raw = read_tmux_log(&handle.tmux_window, 2000).await?;

        // For claude, try to extract JSON result
        if matches!(handle.agent, DelegateAgent::Claude) {
            if let Some(result) = extract_claude_result(&raw) {
                return Ok(result);
            }
        }

        // Strip the [rc: exit=N] marker
        let output = raw
            .lines()
            .filter(|l| !l.starts_with("[rc: exit="))
            .collect::<Vec<_>>()
            .join("\n");

        Ok(output.trim().to_string())
    }

    /// Cancel a delegate.
    pub async fn cancel(&mut self, id: &str) -> Result<String> {
        let handle = self
            .delegates
            .remove(id)
            .ok_or_else(|| anyhow::anyhow!("Delegate '{}' not found", id))?;

        let _ = tokio::process::Command::new("tmux")
            .args([
                "kill-window",
                "-t",
                &format!("rc-bg:{}", handle.tmux_window),
            ])
            .output()
            .await;

        Ok(format!(
            "Cancelled delegate {} ({})",
            id,
            handle.agent.display_name()
        ))
    }

    /// Live tmux log for monitoring a running delegate.
    pub async fn log(&self, id: &str, lines: usize) -> Result<String> {
        let handle = self
            .delegates
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Delegate '{}' not found", id))?;
        read_tmux_log(&handle.tmux_window, lines).await
    }

    async fn check_window_status(&self, window_name: &str) -> DelegateStatus {
        let windows = list_bg_windows().await.unwrap_or_default();
        for (name, done) in &windows {
            if name == window_name {
                return if *done {
                    DelegateStatus::Done
                } else {
                    DelegateStatus::Running
                };
            }
        }
        DelegateStatus::Unknown
    }
}

/// Try to extract the result text from claude --output-format json output.
fn extract_claude_result(raw: &str) -> Option<String> {
    for line in raw.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') && trimmed.contains("\"result\"") {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(result) = v.get("result").and_then(|r| r.as_str()) {
                    return Some(result.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_names() {
        assert!(DelegateAgent::from_name("claude").is_some());
        assert!(DelegateAgent::from_name("Claude").is_some());
        assert!(DelegateAgent::from_name("gemini").is_some());
        assert!(DelegateAgent::from_name("codex").is_some());
        assert!(DelegateAgent::from_name("opencode").is_some());
        assert!(DelegateAgent::from_name("rust-code").is_some());
        assert!(DelegateAgent::from_name("rc").is_some());
        assert!(DelegateAgent::from_name("gpt").is_none());
    }

    #[test]
    fn build_command_claude() {
        let cmd = DelegateAgent::Claude.build_command("fix the bug", Path::new("/tmp/project"));
        assert!(cmd.contains("claude -p"));
        assert!(cmd.contains("--output-format json"));
        assert!(cmd.contains("CLAUDECODE=''"));
        assert!(cmd.contains("cd '/tmp/project'"));
    }

    #[test]
    fn build_command_gemini() {
        let cmd = DelegateAgent::Gemini.build_command("write tests", Path::new("/tmp"));
        assert!(cmd.contains("gemini -p"));
        assert!(cmd.contains("--sandbox"));
    }

    #[test]
    fn build_command_codex() {
        let cmd = DelegateAgent::Codex.build_command("refactor", Path::new("/tmp"));
        assert!(cmd.contains("codex exec"));
        assert!(cmd.contains("--dangerously-bypass-approvals-and-sandbox"));
    }

    #[test]
    fn build_command_opencode() {
        let cmd = DelegateAgent::OpenCode.build_command("analyze", Path::new("/tmp"));
        assert!(cmd.contains("opencode run"));
        assert!(cmd.contains("--format json"));
    }

    #[test]
    fn build_command_rustcode() {
        let cmd = DelegateAgent::RustCode.build_command("build", Path::new("/tmp"));
        assert!(cmd.contains("rust-code -p"));
        assert!(cmd.contains("--loop 5"));
    }

    #[test]
    fn build_task_prompt_with_path() {
        let prompt = build_task_prompt(".tasks/005-fix-bug.md", None);
        assert!(prompt.contains(".tasks/005-fix-bug.md"));
        assert!(prompt.contains("status: done"));
        assert!(prompt.contains("## Results"));
    }

    #[test]
    fn build_task_prompt_with_extra() {
        let prompt = build_task_prompt(".tasks/005.md", Some("focus on tests"));
        assert!(prompt.contains("focus on tests"));
    }

    #[test]
    fn extract_claude_json_result() {
        let raw = r#"some output
{"type":"result","subtype":"success","result":"Fixed the bug","session_id":"abc","cost_usd":0.05}
[rc: exit=0]"#;
        assert_eq!(
            extract_claude_result(raw),
            Some("Fixed the bug".to_string())
        );
    }

    #[test]
    fn extract_claude_no_result() {
        assert_eq!(extract_claude_result("just plain text"), None);
    }

    #[test]
    fn manager_starts_empty() {
        let mgr = DelegateManager::new();
        assert!(mgr.delegates.is_empty());
    }
}
