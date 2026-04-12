//! TreeTool — show directory tree structure.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::{backend_err, def_level, def_root};

pub struct TreeTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct TreeArgs {
    /// Directory path (default: workspace root)
    #[serde(default = "def_root")]
    root: String,
    /// Max depth (default: 2)
    #[serde(default = "def_level")]
    level: i32,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for TreeTool<B> {
    fn name(&self) -> &str {
        "tree"
    }
    fn description(&self) -> &str {
        "Show directory tree structure"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<TreeArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: TreeArgs = parse_args(&args)?;
        self.0
            .tree(&a.root, a.level)
            .await
            .map(ToolOutput::text)
            .map_err(backend_err)
    }
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: TreeArgs = parse_args(&args)?;
        self.0
            .tree(&a.root, a.level)
            .await
            .map(ToolOutput::text)
            .map_err(backend_err)
    }
}
