//! FindTool — find files/directories by name pattern.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::{backend_err, def_root};

pub struct FindTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct FindArgs {
    /// Search root directory
    #[serde(default = "def_root")]
    root: String,
    /// File/directory name pattern
    name: String,
    /// Filter: "files", "dirs", or empty for all
    #[serde(default, rename = "type")]
    file_type: String,
    /// Max results (0 = no limit)
    #[serde(default)]
    limit: i32,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for FindTool<B> {
    fn name(&self) -> &str {
        "find"
    }
    fn description(&self) -> &str {
        "Find files/directories by name pattern"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<FindArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: FindArgs = parse_args(&args)?;
        self.0
            .find(&a.root, &a.name, &a.file_type, a.limit)
            .await
            .map(ToolOutput::text)
            .map_err(backend_err)
    }
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: FindArgs = parse_args(&args)?;
        self.0
            .find(&a.root, &a.name, &a.file_type, a.limit)
            .await
            .map(ToolOutput::text)
            .map_err(backend_err)
    }
}
