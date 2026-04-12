//! Finish and AskUser tools — terminal actions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

// ---------------------------------------------------------------------------
// Finish
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct FinishArgs {
    /// Summary of what was accomplished.
    pub summary: String,
}

pub struct FinishTool;

#[async_trait::async_trait]
impl Tool for FinishTool {
    fn name(&self) -> &str {
        "finish"
    }
    fn description(&self) -> &str {
        "Signal task completion with a summary of what was done."
    }
    fn is_system(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<FinishArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: FinishArgs = parse_args(&args)?;
        Ok(ToolOutput::done(format!("Task finished: {}", args.summary)))
    }
}

// ---------------------------------------------------------------------------
// AskUser
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct AskUserArgs {
    /// Question to ask the user.
    pub question: String,
}

pub struct AskUserTool;

#[async_trait::async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }
    fn description(&self) -> &str {
        "Ask the user a question and wait for their response."
    }
    fn is_system(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<AskUserArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: AskUserArgs = parse_args(&args)?;
        Ok(ToolOutput::waiting(format!(
            "Question for user: {}",
            args.question
        )))
    }
}
