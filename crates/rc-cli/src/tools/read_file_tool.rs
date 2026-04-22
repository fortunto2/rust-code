//! ReadFile tool — reads file contents with caching and re-read warning.

use crate::rc_state::RcState;
use crate::tools::{read_file, truncate_output};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ReadFileArgs {
    /// File path to read.
    pub path: String,
    /// Line offset to start reading from.
    #[serde(default)]
    pub offset: Option<i64>,
    /// Number of lines to read.
    #[serde(default)]
    pub limit: Option<i64>,
}

pub struct ReadFileTool {
    pub state: RcState,
}

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }
    fn description(&self) -> &str {
        "Read file contents. Use offset/limit for large files."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<ReadFileArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ReadFileArgs = parse_args(&args)?;
        let resolved = self.state.resolve_path(&args.path);

        // Check read cache -- return cached content with warning on re-read
        let cache_key = resolved.clone();
        let is_reread = {
            let cache = self.state.read_cache.lock().unwrap();
            cache.contains_key(&cache_key)
        };

        let content = read_file(
            &resolved,
            args.offset.map(|o| o as usize),
            args.limit.map(|l| l as usize),
        )
        .await
        .map_err(ToolError::exec)?;

        let output = if is_reread {
            // Return truncated content on re-read to save context window
            let lines: Vec<&str> = content.lines().collect();
            let preview = if lines.len() > 5 {
                format!(
                    "{}\n... ({} more lines -- use content from conversation history)",
                    lines[..5].join("\n"),
                    lines.len() - 5
                )
            } else {
                content.clone()
            };
            format!(
                "\u{26a0} RE-READ: You already read this file. Content unchanged. \
                 STOP re-reading and ACT on what you already know.\n\
                 Preview (first 5 lines):\n{}",
                preview
            )
        } else {
            format!("File contents of {}:\n{}", args.path, content)
        };

        // Update cache
        {
            let step = self.state.current_step();
            let mut cache = self.state.read_cache.lock().unwrap();
            cache.insert(cache_key, (content, step));
        }

        // Record access for frecency + combo-boost. Only on first read:
        // re-reads would double-count the same file for the same query.
        if !is_reread {
            self.state.fff.track_read(std::path::Path::new(&resolved));
        }

        Ok(ToolOutput::text(truncate_output(&output)))
    }
}
