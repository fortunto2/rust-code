//! ReadAllTool — batch read all files in a directory.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::backend_err;
use crate::trust::infer_trust;

pub struct ReadAllTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct ReadAllArgs {
    /// Directory path to read all files from
    path: String,
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for ReadAllTool<B> {
    fn name(&self) -> &str {
        "read_all"
    }
    fn description(&self) -> &str {
        "Read ALL files in a directory in one call. Much faster than listing then reading one by one. Returns each file with its path header."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<ReadAllArgs>()
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        self.execute_readonly(args, ctx).await
    }
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: ReadAllArgs = parse_args(&args)?;
        let listing = self.0.list(&a.path).await.map_err(backend_err)?;

        let mut output = String::new();
        let mut count = 0u32;
        for line in listing.lines().skip(1) {
            let name = line.trim();
            if name.is_empty() || name.ends_with('/') {
                continue;
            }
            let full_path = if a.path.ends_with('/') {
                format!("{}{}", a.path, name)
            } else {
                format!("{}/{}", a.path, name)
            };
            match self.0.read(&full_path, false, 0, 0).await {
                Ok(content) => {
                    let trust = infer_trust(&full_path);
                    output.push_str(&format!(
                        "--- {} [{}] ---\n{}\n\n",
                        full_path, trust, content
                    ));
                    count += 1;
                }
                Err(e) => {
                    output.push_str(&format!("--- {} ---\n[error: {}]\n\n", full_path, e));
                }
            }
        }
        if count == 0 {
            output.push_str("(no files found)\n");
        }
        Ok(ToolOutput::text(output))
    }
}
