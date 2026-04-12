//! DeleteTool — delete one or more files (batch delete support).

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;

pub struct DeleteTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct DeleteArgs {
    /// File path to delete (use this for single file)
    #[serde(default)]
    path: Option<String>,
    /// Multiple file paths to delete in one call (preferred for bulk cleanup)
    #[serde(default)]
    paths: Option<Vec<String>>,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for DeleteTool<B> {
    fn name(&self) -> &str {
        "delete"
    }
    fn description(&self) -> &str {
        "Delete one or more files. Pass `path` for a single file, or `paths` (array) to delete many files at once."
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<DeleteArgs>()
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: DeleteArgs = parse_args(&args)?;

        let targets: Vec<String> = match (a.path, a.paths) {
            (_, Some(ps)) if !ps.is_empty() => ps,
            (Some(p), _) => vec![p],
            _ => {
                return Err(ToolError::InvalidArgs(
                    "provide `path` (string) or `paths` (array)".into(),
                ));
            }
        };

        let mut results: Vec<String> = Vec::with_capacity(targets.len());
        let mut errors: Vec<String> = Vec::new();

        for path in &targets {
            match self.0.delete(path).await {
                Ok(()) => results.push(format!("Deleted {}", path)),
                Err(e) => errors.push(format!("FAILED {}: {}", path, e)),
            }
        }

        let mut out = results.join("\n");
        if !errors.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&errors.join("\n"));
        }
        if out.is_empty() {
            out = "No files deleted".to_string();
        }
        Ok(ToolOutput::text(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_fs::MockFs;
    use sgr_agent_core::agent_tool::Tool;

    #[tokio::test]
    async fn test_single_delete() {
        let fs = Arc::new(MockFs::new());
        fs.add_file("tmp.txt", "bye");
        let tool = DeleteTool(fs.clone());
        let mut ctx = AgentContext::new();
        let result = tool
            .execute(serde_json::json!({"path": "tmp.txt"}), &mut ctx)
            .await
            .unwrap();
        assert!(result.content.contains("Deleted tmp.txt"));
        assert!(!fs.exists("tmp.txt"));
    }

    #[tokio::test]
    async fn test_batch_delete() {
        let fs = Arc::new(MockFs::new());
        fs.add_file("a.txt", "1");
        fs.add_file("b.txt", "2");
        let tool = DeleteTool(fs.clone());
        let mut ctx = AgentContext::new();
        let result = tool
            .execute(serde_json::json!({"paths": ["a.txt", "b.txt"]}), &mut ctx)
            .await
            .unwrap();
        assert!(result.content.contains("Deleted a.txt"));
        assert!(result.content.contains("Deleted b.txt"));
        assert!(!fs.exists("a.txt"));
        assert!(!fs.exists("b.txt"));
    }

    #[tokio::test]
    async fn test_delete_nonexistent() {
        let fs = Arc::new(MockFs::new());
        let tool = DeleteTool(fs.clone());
        let mut ctx = AgentContext::new();
        let result = tool
            .execute(serde_json::json!({"path": "ghost.txt"}), &mut ctx)
            .await
            .unwrap();
        assert!(result.content.contains("FAILED ghost.txt"));
    }
}
