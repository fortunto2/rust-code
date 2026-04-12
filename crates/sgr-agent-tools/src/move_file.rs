//! MoveTool — move or rename a file.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;

pub struct MoveTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct MoveArgs {
    /// Source file path
    from: String,
    /// Destination file path
    to: String,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for MoveTool<B> {
    fn name(&self) -> &str {
        "move_file"
    }
    fn description(&self) -> &str {
        "Move or rename a file"
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<MoveArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: MoveArgs = parse_args(&args)?;
        self.0
            .move_file(&a.from, &a.to)
            .await
            .map_err(backend_err)?;
        Ok(ToolOutput::text(format!("Moved {} → {}", a.from, a.to)))
    }
}
