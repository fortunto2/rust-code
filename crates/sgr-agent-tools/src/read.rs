//! ReadTool — read file contents with trust metadata.
//!
//! Core read logic without workflow guards or content scanning.
//! For PAC1-specific behavior (workflow tracking, guard_content), wrap this tool.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;
use crate::trust::wrap_with_meta;

pub struct ReadTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct ReadArgs {
    /// File path
    path: String,
    /// Show line numbers (like cat -n)
    #[serde(default)]
    number: bool,
    /// Start line (1-indexed, like sed)
    #[serde(default)]
    start_line: i32,
    #[serde(default)]
    end_line: i32,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for ReadTool<B> {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read file contents. Use number=true to see line numbers (like cat -n). \
         Use start_line/end_line to read a specific range (like sed -n '5,10p'). \
         For large files: first read with number=true, then read specific ranges."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<ReadArgs>()
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        self.execute_readonly(args, ctx).await
    }
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: ReadArgs = parse_args(&args)?;
        let result = self
            .0
            .read(&a.path, a.number, a.start_line, a.end_line)
            .await
            .map_err(backend_err)?;
        Ok(ToolOutput::text(wrap_with_meta(&a.path, &result)))
    }
}
