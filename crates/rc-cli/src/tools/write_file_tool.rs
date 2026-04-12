//! WriteFile tool — creates or overwrites a file with new content.

use crate::rc_state::RcState;
use crate::tools::write_file;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct WriteFileArgs {
    /// File path to write.
    pub path: String,
    /// File content.
    pub content: String,
}

pub struct WriteFileTool {
    pub state: RcState,
}

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Create or overwrite a file with new content."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<WriteFileArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: WriteFileArgs = parse_args(&args)?;
        let resolved = self.state.resolve_path(&args.path);
        let is_new = !std::path::Path::new(&resolved).exists();
        write_file(&resolved, &args.content)
            .await
            .map_err(ToolError::exec)?;
        // Invalidate read cache for this file
        self.state.read_cache.lock().unwrap().remove(&resolved);
        let label = if is_new { "Created" } else { "Wrote" };
        let lines = args.content.lines().count();
        Ok(ToolOutput::text(format!(
            "{} {} ({} lines)",
            label, args.path, lines
        )))
    }
}
