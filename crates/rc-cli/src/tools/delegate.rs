//! Delegate tasks to external CLI agents (claude, gemini, codex) running
//! as full autonomous agents in tmux background.
//!
//! The orchestrator (rust-code on Gemini) gives them a task and collects results.
//! Unlike the swarm system (in-process sub-agents), delegates are independent
//! CLI processes with their own auth, tools, and multi-step capabilities.

use super::bash::{list_bg_windows, read_tmux_log, run_command_bg};
use anyhow::Result;
use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

/// Supported delegate agents.
#[derive(Debug, Clone, Copy)]
pub enum DelegateAgent {
    Claude,
    Gemini,
    Codex,
    RustCode,
}

impl DelegateAgent {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "claude" | "claude-code" => Some(Self::Claude),
            "gemini" | "gemini-cli" => Some(Self::Gemini),
            "codex" | "codex-cli" => Some(Self::Codex),
            "rust-code" | "rustcode" | "rc" => Some(Self::RustCode),
            _ => None,
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Codex => "codex",
            Self::RustCode => "rust-code",
        }
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
            Self::RustCode => {
                format!("{cd} && rust-code -p '{escaped_task}' --loop 5")
            }
        }
    }
}

/// Handle for a running delegate.
pub struct DelegateHandle {
    pub agent: DelegateAgent,
    pub task: String,
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
    pub async fn spawn(&mut self, agent: DelegateAgent, task: &str, cwd: &Path) -> Result<String> {
        self.counter += 1;
        let id = format!("del-{}", self.counter);
        let window_name = id.clone();

        let command = agent.build_command(task, cwd);
        run_command_bg(&window_name, &command).await?;

        self.delegates.insert(
            id.clone(),
            DelegateHandle {
                agent,
                task: task.to_string(),
                tmux_window: window_name,
                started_at: Instant::now(),
            },
        );

        Ok(id)
    }

    /// Check status of a specific delegate.
    pub async fn status(&self, id: &str) -> Option<(DelegateStatus, std::time::Duration)> {
        let handle = self.delegates.get(id)?;
        let elapsed = handle.started_at.elapsed();
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
                let status = match window_done.get(handle.tmux_window.as_str()) {
                    Some(true) => DelegateStatus::Done,
                    Some(false) => DelegateStatus::Running,
                    None => DelegateStatus::Unknown,
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

    /// Get output from a delegate (reads tmux buffer).
    pub async fn result(&self, id: &str) -> Result<String> {
        let handle = self
            .delegates
            .get(id)
            .ok_or_else(|| anyhow::anyhow!("Delegate '{}' not found", id))?;

        let raw = read_tmux_log(&handle.tmux_window, 2000).await?;

        // For claude with --output-format json, try to extract result
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
    // Claude JSON output format: {"type":"result","subtype":"success","result":"..."}
    // Find the last JSON line that has "result"
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
    fn extract_claude_json_result() {
        let raw = r#"some output
{"type":"result","subtype":"success","result":"Fixed the bug in main.rs","session_id":"abc123","cost_usd":0.05}
[rc: exit=0]"#;
        let result = extract_claude_result(raw);
        assert_eq!(result, Some("Fixed the bug in main.rs".to_string()));
    }

    #[test]
    fn extract_claude_no_result() {
        let result = extract_claude_result("just plain text output");
        assert_eq!(result, None);
    }

    #[test]
    fn manager_starts_empty() {
        let mgr = DelegateManager::new();
        assert!(mgr.delegates.is_empty());
    }
}
