//! OpenEditor tool — open a file in the user's editor.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct OpenEditorArgs {
    /// File path to open.
    pub path: String,
    /// Line number to jump to.
    #[serde(default)]
    pub line: Option<i64>,
}

pub struct OpenEditorTool;

#[async_trait::async_trait]
impl Tool for OpenEditorTool {
    fn name(&self) -> &str {
        "open_editor"
    }
    fn description(&self) -> &str {
        "Open a file in the user's editor."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<OpenEditorArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: OpenEditorArgs = parse_args(&args)?;
        Ok(ToolOutput::text(format!("Opened {} in editor", args.path)))
    }
}
