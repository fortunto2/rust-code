//! WriteTool — write content to a file with JSON auto-repair.
//!
//! Core write logic: JSON repair for .json files via llm_json.
//! PAC1-specific behavior (outbox injection, README schema validation, workflow guards)
//! should be added via wrapping or hooks at the call site.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;

pub struct WriteTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct WriteArgs {
    /// File path
    path: String,
    /// File content to write
    content: String,
    /// Start line for ranged overwrite (1-indexed)
    #[serde(default)]
    start_line: i32,
    /// End line for ranged overwrite
    #[serde(default)]
    end_line: i32,
}

/// Auto-repair broken JSON content via llm_json.
/// Returns repaired content or original if not JSON / already valid.
fn maybe_repair_json(path: &str, content: &str) -> String {
    if !path.ends_with(".json") {
        return content.to_string();
    }
    match serde_json::from_str::<serde_json::Value>(content) {
        Ok(_) => content.to_string(),
        Err(_) => {
            let opts = llm_json::RepairOptions::default();
            llm_json::repair_json(content, &opts).unwrap_or_else(|_| content.to_string())
        }
    }
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for WriteTool<B> {
    fn name(&self) -> &str {
        "write"
    }
    fn description(&self) -> &str {
        "Write content to a file. Without start_line/end_line: overwrites entire file. \
         With start_line and end_line: replaces only those lines (like sed). \
         Example: start_line=5, end_line=7 replaces lines 5-7 with content. \
         Use read with number=true first to see line numbers."
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<WriteArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: WriteArgs = parse_args(&args)?;
        let content = maybe_repair_json(&a.path, &a.content);

        self.0
            .write(&a.path, &content, a.start_line, a.end_line)
            .await
            .map_err(backend_err)?;

        let msg = if a.start_line > 0 && a.end_line > 0 {
            format!(
                "Replaced lines {}-{} in {}",
                a.start_line, a.end_line, a.path
            )
        } else if a.start_line > 0 {
            format!("Replaced from line {} in {}", a.start_line, a.path)
        } else {
            format!("Written to {}", a.path)
        };
        Ok(ToolOutput::text(msg))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_fs::MockFs;
    use sgr_agent_core::agent_tool::Tool;

    #[tokio::test]
    async fn test_write_new_file() {
        let fs = Arc::new(MockFs::new());
        let tool = WriteTool(fs.clone());
        let mut ctx = AgentContext::new();
        let result = tool
            .execute(
                serde_json::json!({"path": "out.txt", "content": "hello"}),
                &mut ctx,
            )
            .await
            .unwrap();
        assert!(result.content.contains("Written to out.txt"));
        assert_eq!(fs.content("out.txt").unwrap(), "hello");
    }

    #[tokio::test]
    async fn test_write_json_repair() {
        let fs = Arc::new(MockFs::new());
        let tool = WriteTool(fs.clone());
        let mut ctx = AgentContext::new();
        // Broken JSON: missing closing brace
        let result = tool
            .execute(
                serde_json::json!({"path": "data.json", "content": "{\"key\": \"value\""}),
                &mut ctx,
            )
            .await
            .unwrap();
        assert!(result.content.contains("Written to data.json"));
        let stored = fs.content("data.json").unwrap();
        // Repaired JSON should be valid
        assert!(serde_json::from_str::<serde_json::Value>(&stored).is_ok());
    }
}
