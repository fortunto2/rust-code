//! ShellTool — execute shell commands via tokio::process.
//!
//! Requires the `shell` feature flag (needs tokio "process" feature).

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

pub struct ShellTool;

/// Maximum output size in bytes (100KB).
const MAX_OUTPUT_BYTES: usize = 100 * 1024;

/// Default timeout in milliseconds (2 minutes).
const DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// Maximum allowed timeout in milliseconds (10 minutes).
const MAX_TIMEOUT_MS: u64 = 600_000;

#[derive(Deserialize, JsonSchema)]
struct ShellArgs {
    /// Shell command to execute
    command: String,
    /// Working directory (default: current)
    #[serde(default)]
    workdir: Option<String>,
    /// Timeout in milliseconds (default: 120000, max: 600000)
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// Detect the user's shell from SHELL env var, fallback to /bin/sh.
fn detect_shell() -> String {
    std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
}

#[async_trait::async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }
    fn description(&self) -> &str {
        "Execute a shell command. Returns exit code and combined stdout+stderr output. \
         Use workdir to set working directory. Default timeout: 120s, max: 600s."
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<ShellArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: ShellArgs = parse_args(&args)?;

        let timeout_ms = a
            .timeout_ms
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);

        let shell = detect_shell();
        let mut cmd = tokio::process::Command::new(&shell);
        cmd.arg("-c").arg(&a.command);

        if let Some(ref dir) = a.workdir {
            cmd.current_dir(dir);
        }

        // Merge stderr into stdout
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());

        let timeout_dur = std::time::Duration::from_millis(timeout_ms);

        let result = tokio::time::timeout(timeout_dur, cmd.output()).await;

        match result {
            Ok(Ok(output)) => {
                let code = output.status.code().unwrap_or(-1);
                let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = String::from_utf8_lossy(&output.stderr);
                if !stderr.is_empty() {
                    if !combined.is_empty() {
                        combined.push('\n');
                    }
                    combined.push_str(&stderr);
                }

                // Truncate to MAX_OUTPUT_BYTES
                if combined.len() > MAX_OUTPUT_BYTES {
                    combined.truncate(MAX_OUTPUT_BYTES);
                    combined.push_str("\n... (output truncated to 100KB)");
                }

                Ok(ToolOutput::text(format!(
                    "Exit code: {}\n\nOutput:\n{}",
                    code, combined
                )))
            }
            Ok(Err(e)) => Err(ToolError::Execution(format!(
                "Failed to execute command: {}",
                e
            ))),
            Err(_) => Ok(ToolOutput::text(format!(
                "Command timed out after {}ms",
                timeout_ms
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_shell_returns_something() {
        let shell = detect_shell();
        assert!(!shell.is_empty());
    }

    #[tokio::test]
    async fn shell_echo() {
        let tool = ShellTool;
        let mut ctx = AgentContext::new();
        let args = serde_json::json!({"command": "echo hello"});
        let output = tool.execute(args, &mut ctx).await.unwrap();
        assert!(output.content.contains("Exit code: 0"));
        assert!(output.content.contains("hello"));
    }

    #[tokio::test]
    async fn shell_exit_code() {
        let tool = ShellTool;
        let mut ctx = AgentContext::new();
        let args = serde_json::json!({"command": "exit 42"});
        let output = tool.execute(args, &mut ctx).await.unwrap();
        assert!(output.content.contains("Exit code: 42"));
    }

    #[tokio::test]
    async fn shell_stderr() {
        let tool = ShellTool;
        let mut ctx = AgentContext::new();
        let args = serde_json::json!({"command": "echo err >&2"});
        let output = tool.execute(args, &mut ctx).await.unwrap();
        assert!(output.content.contains("err"));
    }

    #[tokio::test]
    async fn shell_timeout() {
        let tool = ShellTool;
        let mut ctx = AgentContext::new();
        let args = serde_json::json!({"command": "sleep 10", "timeout_ms": 100});
        let output = tool.execute(args, &mut ctx).await.unwrap();
        assert!(output.content.contains("timed out"));
    }

    #[tokio::test]
    async fn shell_workdir() {
        let tool = ShellTool;
        let mut ctx = AgentContext::new();
        let args = serde_json::json!({"command": "pwd", "workdir": "/tmp"});
        let output = tool.execute(args, &mut ctx).await.unwrap();
        assert!(output.content.contains("/tmp") || output.content.contains("/private/tmp"));
    }
}
