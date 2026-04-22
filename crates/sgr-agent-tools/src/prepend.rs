//! PrependTool — prepend header text to a file without touching the body.
//!
//! Reads the file, prepends header, writes back. Body is byte-perfect —
//! never passes through LLM context. Used for adding YAML frontmatter.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;

pub struct PrependTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct PrependArgs {
    /// File path to prepend to
    path: String,
    /// Text to prepend (e.g. YAML frontmatter block). Will be followed by a newline before existing content.
    header: String,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for PrependTool<B> {
    fn name(&self) -> &str {
        "prepend_to_file"
    }
    fn description(&self) -> &str {
        "Prepend header text to a file. Body is preserved byte-for-byte (never re-typed). \
         Use for adding YAML frontmatter to existing files without risking body corruption."
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<PrependArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: PrependArgs = parse_args(&args)?;

        // Read existing content
        let raw = self
            .0
            .read(&a.path, false, 0, 0)
            .await
            .map_err(backend_err)?;
        // Strip PCM header ("$ cat path\n") if present
        let body = if raw.starts_with("$ ") {
            raw.find('\n').map(|i| &raw[i + 1..]).unwrap_or(&raw)
        } else {
            &raw
        };

        // Prepend header + newline + original body
        let header = a.header.trim_end_matches('\n');
        let combined = format!("{}\n{}", header, body);

        self.0
            .write(&a.path, &combined, 0, 0)
            .await
            .map_err(backend_err)?;

        Ok(ToolOutput::text(format!(
            "Prepended {} bytes to {} (body preserved)",
            header.len(),
            a.path
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_fs::MockFs;
    use sgr_agent_core::agent_tool::Tool;

    #[tokio::test]
    async fn test_prepend_frontmatter() {
        let fs = Arc::new(MockFs::new());
        fs.add_file("doc.md", "# My Document\n\nSome content here.");
        let tool = PrependTool(fs.clone());
        let mut ctx = AgentContext::new();
        let result = tool
            .execute(
                serde_json::json!({
                    "path": "doc.md",
                    "header": "---\ntitle: My Document\ntype: note\n---"
                }),
                &mut ctx,
            )
            .await
            .unwrap();
        assert!(result.content.contains("Prepended"));
        let content = fs.content("doc.md").unwrap();
        assert!(content.starts_with("---\ntitle: My Document"));
        assert!(content.contains("# My Document\n\nSome content here."));
    }

    #[tokio::test]
    async fn test_prepend_to_empty() {
        let fs = Arc::new(MockFs::new());
        fs.add_file("empty.md", "");
        let tool = PrependTool(fs.clone());
        let mut ctx = AgentContext::new();
        let _ = tool
            .execute(
                serde_json::json!({"path": "empty.md", "header": "---\ntitle: New\n---"}),
                &mut ctx,
            )
            .await
            .unwrap();
        let content = fs.content("empty.md").unwrap();
        assert!(content.starts_with("---\ntitle: New\n---"));
    }
}
