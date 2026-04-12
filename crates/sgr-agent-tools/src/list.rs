//! ListTool — list directory contents.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;

pub struct ListTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct ListArgs {
    /// Directory path
    path: String,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for ListTool<B> {
    fn name(&self) -> &str {
        "list"
    }
    fn description(&self) -> &str {
        "List directory contents"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<ListArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: ListArgs = parse_args(&args)?;
        self.0
            .list(&a.path)
            .await
            .map(ToolOutput::text)
            .map_err(backend_err)
    }
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: ListArgs = parse_args(&args)?;
        self.0
            .list(&a.path)
            .await
            .map(ToolOutput::text)
            .map_err(backend_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_fs::MockFs;
    use sgr_agent_core::agent_tool::Tool;

    #[tokio::test]
    async fn test_list_directory() {
        let fs = Arc::new(MockFs::new());
        fs.add_file("docs/a.txt", "aaa");
        fs.add_file("docs/b.txt", "bbb");
        fs.add_file("src/main.rs", "fn main() {}");
        let tool = ListTool(fs.clone());
        let ctx = AgentContext::new();
        let result = tool
            .execute_readonly(serde_json::json!({"path": "docs"}), &ctx)
            .await
            .unwrap();
        assert!(result.content.contains("a.txt"));
        assert!(result.content.contains("b.txt"));
        assert!(!result.content.contains("main.rs"));
    }
}
