//! Git tools — status, diff, add, commit.

use crate::rc_state::RcState;
use crate::tools::{git_add, git_diff, git_status, truncate_output};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

// ---------------------------------------------------------------------------
// GitStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GitStatusArgs {}

pub struct GitStatusTool;

#[async_trait::async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str {
        "git_status"
    }
    fn description(&self) -> &str {
        "Show git status of the working directory."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<GitStatusArgs>()
    }

    async fn execute(
        &self,
        _args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        match git_status().map_err(ToolError::exec)? {
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
                Ok(ToolOutput::text(result))
            }
            None => Ok(ToolOutput::text("Not in a git repository")),
        }
    }
}

// ---------------------------------------------------------------------------
// GitDiff
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GitDiffArgs {
    /// Optional path to diff.
    #[serde(default)]
    pub path: Option<String>,
    /// Show staged changes only.
    #[serde(default)]
    pub cached: Option<bool>,
}

pub struct GitDiffTool;

#[async_trait::async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str {
        "git_diff"
    }
    fn description(&self) -> &str {
        "Show git diff. Use cached=true for staged changes."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<GitDiffArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitDiffArgs = parse_args(&args)?;
        let diff = git_diff(args.path.as_deref(), args.cached.unwrap_or(false))
            .map_err(ToolError::exec)?;
        let output = if diff.is_empty() {
            "No changes to show".into()
        } else {
            format!("Git Diff:\n{}", diff)
        };
        Ok(ToolOutput::text(output))
    }
}

// ---------------------------------------------------------------------------
// GitAdd
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GitAddArgs {
    /// File paths to stage.
    pub paths: Vec<String>,
}

pub struct GitAddTool;

#[async_trait::async_trait]
impl Tool for GitAddTool {
    fn name(&self) -> &str {
        "git_add"
    }
    fn description(&self) -> &str {
        "Stage files for commit."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<GitAddArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitAddArgs = parse_args(&args)?;
        git_add(&args.paths).map_err(ToolError::exec)?;
        Ok(ToolOutput::text(format!(
            "Added {} files to staging",
            args.paths.len()
        )))
    }
}

// ---------------------------------------------------------------------------
// GitCommit
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct GitCommitArgs {
    /// Commit message.
    pub message: String,
}

pub struct GitCommitTool {
    pub state: RcState,
}

#[async_trait::async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str {
        "git_commit"
    }
    fn description(&self) -> &str {
        "Create a git commit with a message."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<GitCommitArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: GitCommitArgs = parse_args(&args)?;
        let cwd = self.state.cwd.lock().unwrap().clone();
        // Try commit -- if pre-commit hook fails, return full error output
        // so agent can read it, fix the issue (fmt/test/clippy), and retry.
        let r = sgr_agent::app_tools::bash::run_command_in(
            &format!("git commit -m '{}'", args.message.replace('\'', "'\\''")),
            &cwd,
            Some(120_000), // 2 min timeout for pre-commit hook
        )
        .await;
        if r.exit_code == 0 {
            Ok(ToolOutput::text(format!(
                "Committed: {}\n{}",
                args.message,
                truncate_output(&r.output)
            )))
        } else {
            Ok(ToolOutput::text(format!(
                "Commit FAILED (exit {}). Fix the errors below, then retry git_commit.\n\n{}\n\n\
                 HINT: If format check failed, run the formatter (e.g. `make fmt`). \
                 If tests failed, fix the test. If lint failed, fix the warning. \
                 Then git_add the fixes and git_commit again.",
                r.exit_code,
                truncate_output(&r.output)
            )))
        }
    }
}
