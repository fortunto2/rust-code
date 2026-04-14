//! CopyTool — copy a file without LLM in the loop (byte-perfect).

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;

pub struct CopyTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct CopyArgs {
    /// Source file path
    source: String,
    /// Destination file path (can be same as source for in-place rewrite)
    target: String,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for CopyTool<B> {
    fn name(&self) -> &str {
        "copy_file"
    }
    fn description(&self) -> &str {
        "Copy a file byte-for-byte. Use instead of read+write when content must be preserved verbatim (long docs, invoices, migration)"
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<CopyArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: CopyArgs = parse_args(&args)?;
        let content = self
            .0
            .read(&a.source, false, 0, 0)
            .await
            .map_err(backend_err)?;
        // Strip PCM header ("$ cat path\n") if present
        let body = if content.starts_with("$ ") {
            content
                .find('\n')
                .map(|i| &content[i + 1..])
                .unwrap_or(&content)
        } else {
            &content
        };
        self.0
            .write(&a.target, body, 0, 0)
            .await
            .map_err(backend_err)?;
        let bytes = body.len();
        Ok(ToolOutput::text(format!(
            "Copied {} → {} ({} bytes)",
            a.source, a.target, bytes
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_fs::MockFs;
    use sgr_agent_core::agent_tool::Tool;

    #[tokio::test]
    async fn test_copy_file() {
        let fs = Arc::new(MockFs::new());
        fs.add_file("src.md", "# Hello\n\nLong content here...");
        let tool = CopyTool(fs.clone());
        let mut ctx = AgentContext::new();
        let result = tool
            .execute(
                serde_json::json!({"source": "src.md", "target": "dst.md"}),
                &mut ctx,
            )
            .await
            .unwrap();
        assert!(result.content.contains("Copied src.md → dst.md"));
        assert!(fs.content("dst.md").unwrap().starts_with("# Hello\n\nLong content here..."));
        // Source preserved
        assert!(fs.exists("src.md"));
    }

    #[tokio::test]
    async fn test_copy_in_place() {
        let fs = Arc::new(MockFs::new());
        fs.add_file("doc.md", "original content");
        let tool = CopyTool(fs.clone());
        let mut ctx = AgentContext::new();
        let result = tool
            .execute(
                serde_json::json!({"source": "doc.md", "target": "doc.md"}),
                &mut ctx,
            )
            .await
            .unwrap();
        assert!(result.content.contains("Copied doc.md → doc.md"));
        assert!(fs.content("doc.md").unwrap().starts_with("original content"));
    }
}
