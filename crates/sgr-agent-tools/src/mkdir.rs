//! MkDirTool — create a directory.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;

pub struct MkDirTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct MkDirArgs {
    /// Directory path to create
    path: String,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for MkDirTool<B> {
    fn name(&self) -> &str {
        "mkdir"
    }
    fn description(&self) -> &str {
        "Create a directory"
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<MkDirArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: MkDirArgs = parse_args(&args)?;
        self.0.mkdir(&a.path).await.map_err(backend_err)?;
        Ok(ToolOutput::text(format!("Created directory {}", a.path)))
    }
}
