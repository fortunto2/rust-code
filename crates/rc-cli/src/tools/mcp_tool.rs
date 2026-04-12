//! MCP tool — call tools on MCP servers.

use crate::tools::mcp::McpManager;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct McpCallArgs {
    /// MCP server name.
    pub server: String,
    /// Tool name on the server.
    pub tool: String,
    /// JSON-encoded arguments.
    #[serde(default)]
    pub arguments: Option<String>,
}

pub struct McpCallTool {
    pub mcp: Arc<Option<McpManager>>,
}

#[async_trait::async_trait]
impl Tool for McpCallTool {
    fn name(&self) -> &str {
        "mcp_call"
    }
    fn description(&self) -> &str {
        "Call a tool on an MCP server."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<McpCallArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: McpCallArgs = parse_args(&args)?;
        let Some(mcp) = self.mcp.as_ref() else {
            return Ok(ToolOutput::text("MCP not initialized. No .mcp.json found."));
        };
        let parsed_args = args.arguments.as_ref().and_then(|json_str| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(json_str).ok()
        });
        match mcp.call_tool(&args.server, &args.tool, parsed_args).await {
            Ok(result) => {
                let output = crate::tools::mcp::format_tool_result(&result);
                Ok(ToolOutput::text(format!(
                    "MCP [{}] {}:\n{}",
                    args.server, args.tool, output
                )))
            }
            Err(e) => Ok(ToolOutput::text(format!(
                "MCP Error [{}] {}: {}",
                args.server, args.tool, e
            ))),
        }
    }
}
