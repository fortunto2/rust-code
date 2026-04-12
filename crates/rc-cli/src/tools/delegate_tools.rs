//! Delegate tools — delegate tasks to external CLI agents.

use crate::rc_state::RcState;
use crate::tools::delegate::{DelegateAgent, DelegateManager};
use crate::tools::truncate_output;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

// ---------------------------------------------------------------------------
// DelegateTask
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DelegateTaskArgs {
    /// CLI agent: "claude", "gemini", "codex", "opencode", "rust-code".
    pub agent: String,
    /// Free-text task description. Optional if task_path is given.
    #[serde(default)]
    pub task: Option<String>,
    /// Path to a .tasks/ file. Agent reads it, executes, updates status to done.
    #[serde(default)]
    pub task_path: Option<String>,
    /// Working directory (default: current cwd).
    #[serde(default)]
    pub cwd: Option<String>,
}

pub struct DelegateTaskTool {
    pub state: RcState,
    pub delegate_mgr: Arc<TokioMutex<DelegateManager>>,
}

#[async_trait::async_trait]
impl Tool for DelegateTaskTool {
    fn name(&self) -> &str {
        "delegate_task"
    }
    fn description(&self) -> &str {
        "Delegate a task to a CLI agent (claude/gemini/codex/opencode/rust-code). \
         Give either a free-text 'task' or a 'task_path' to a .tasks/ file. \
         When task_path is used, the agent reads the file, executes, and updates status to done. \
         Runs in tmux background. The agent inherits CLAUDE.md and project context automatically."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<DelegateTaskArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DelegateTaskArgs = parse_args(&args)?;
        let delegate_agent = match DelegateAgent::from_name(&args.agent) {
            Some(a) => a,
            None => {
                return Ok(ToolOutput::text(format!(
                    "Unknown delegate agent: '{}'. Use: claude, gemini, codex, opencode, rust-code",
                    args.agent
                )));
            }
        };
        let work_dir = args
            .cwd
            .as_ref()
            .map(|p| std::path::PathBuf::from(self.state.resolve_path(p)))
            .unwrap_or_else(|| self.state.cwd.lock().unwrap().clone());

        let mut mgr = self.delegate_mgr.lock().await;
        match mgr
            .spawn(
                delegate_agent,
                args.task.as_deref(),
                args.task_path.as_deref(),
                &work_dir,
            )
            .await
        {
            Ok(id) => {
                let label = args
                    .task_path
                    .as_deref()
                    .unwrap_or(args.task.as_deref().unwrap_or("(no task)"));
                Ok(ToolOutput::text(format!(
                    "Delegated to {} (id: {})\nTask: {}\nCwd: {}\n\n\
                     Use delegate_status to check progress, delegate_result to get output.",
                    args.agent,
                    id,
                    label,
                    work_dir.display()
                )))
            }
            Err(e) => Ok(ToolOutput::text(format!("Failed to delegate: {}", e))),
        }
    }
}

// ---------------------------------------------------------------------------
// DelegateStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DelegateStatusArgs {
    /// Delegate ID to check. If omitted, shows all delegates.
    #[serde(default)]
    pub id: Option<String>,
}

pub struct DelegateStatusTool {
    pub delegate_mgr: Arc<TokioMutex<DelegateManager>>,
}

#[async_trait::async_trait]
impl Tool for DelegateStatusTool {
    fn name(&self) -> &str {
        "delegate_status"
    }
    fn description(&self) -> &str {
        "Check status of delegated tasks (running/done). Omit id to see all."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<DelegateStatusArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DelegateStatusArgs = parse_args(&args)?;
        let mgr = self.delegate_mgr.lock().await;
        if let Some(id) = &args.id {
            match mgr.status(id).await {
                Some((status, elapsed)) => Ok(ToolOutput::text(format!(
                    "[{}] {} ({}s elapsed)",
                    id,
                    status,
                    elapsed.as_secs()
                ))),
                None => Ok(ToolOutput::text(format!("Delegate '{}' not found", id))),
            }
        } else {
            let all = mgr.status_all().await;
            if all.is_empty() {
                return Ok(ToolOutput::text("No delegates running."));
            }
            let output = all
                .iter()
                .map(|(id, agent, status, elapsed)| {
                    format!(
                        "[{}] {} \u{2014} {} ({}s)",
                        id,
                        agent,
                        status,
                        elapsed.as_secs()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(ToolOutput::text(output))
        }
    }
}

// ---------------------------------------------------------------------------
// DelegateResult
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct DelegateResultArgs {
    /// Delegate ID to get results from.
    pub id: String,
}

pub struct DelegateResultTool {
    pub delegate_mgr: Arc<TokioMutex<DelegateManager>>,
}

#[async_trait::async_trait]
impl Tool for DelegateResultTool {
    fn name(&self) -> &str {
        "delegate_result"
    }
    fn description(&self) -> &str {
        "Get the output from a completed delegate."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<DelegateResultArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: DelegateResultArgs = parse_args(&args)?;
        let mgr = self.delegate_mgr.lock().await;
        match mgr.result(&args.id).await {
            Ok(output) => Ok(ToolOutput::text(truncate_output(&format!(
                "[{}] result:\n{}",
                args.id, output
            )))),
            Err(e) => Ok(ToolOutput::text(format!(
                "Error getting result for {}: {}",
                args.id, e
            ))),
        }
    }
}
