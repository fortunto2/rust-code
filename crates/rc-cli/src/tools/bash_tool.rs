//! Bash and BashBg tools — run shell commands with CWD tracking.

use crate::rc_state::RcState;
use crate::tools::truncate_output;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

// ---------------------------------------------------------------------------
// Bash (foreground)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct BashArgs {
    /// Shell command to execute.
    pub command: String,
    /// Human-readable description of what this command does.
    #[serde(default)]
    pub description: Option<String>,
    /// Timeout in milliseconds.
    #[serde(default)]
    pub timeout: Option<i64>,
}

pub struct BashTool {
    pub state: RcState,
}

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> &str {
        "Run a shell command and return stdout/stderr."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<BashArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: BashArgs = parse_args(&args)?;
        let timeout_ms = args.timeout.map(|t| (t as u64).min(600_000));
        let current_cwd = self.state.cwd.lock().unwrap().clone();
        let result = crate::tools::run_command_in(&args.command, &current_cwd, timeout_ms).await;
        *self.state.cwd.lock().unwrap() = result.cwd;
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
        Ok(ToolOutput::text(output_text))
    }
}

// ---------------------------------------------------------------------------
// BashBg (background, tmux)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct BashBgArgs {
    /// Background task name.
    pub name: String,
    /// Shell command to run in background.
    pub command: String,
}

pub struct BashBgTool;

#[async_trait::async_trait]
impl Tool for BashBgTool {
    fn name(&self) -> &str {
        "bash_bg"
    }
    fn description(&self) -> &str {
        "Run a shell command in background (tmux)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<BashBgArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: BashBgArgs = parse_args(&args)?;
        let output = crate::tools::run_command_bg(&args.name, &args.command)
            .await
            .map_err(ToolError::exec)?;
        Ok(ToolOutput::text(format!("[BG] {}", output)))
    }
}
