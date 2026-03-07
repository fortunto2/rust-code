use anyhow::{Result, anyhow};
use rmcp::{
    ServiceExt,
    model::{CallToolRequestParam, CallToolResult, Tool},
    service::RunningService,
    transport::TokioChildProcess,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::process::Command;

// --- Config (matches .mcp.json format used by Claude Code) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(rename = "mcpServers")]
    pub mcp_servers: HashMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
}

// --- Domain types ---

#[derive(Debug, Clone)]
pub struct McpTool {
    pub server_name: String,
    pub tool: Tool,
}

type McpService = RunningService<rmcp::RoleClient, ()>;

struct McpServer {
    name: String,
    service: McpService,
    tools: Vec<Tool>,
}

// --- McpManager: owns server lifecycles and tool dispatch ---

pub struct McpManager {
    servers: Vec<McpServer>,
}

impl McpManager {
    /// Load and merge .mcp.json from ~/.mcp.json (global) and ./.mcp.json (project).
    /// Project config overrides global for same server name.
    pub fn load_configs() -> McpConfig {
        let mut merged = HashMap::new();

        if let Ok(home) = std::env::var("HOME") {
            if let Ok(cfg) = load_config_file(&PathBuf::from(&home).join(".mcp.json")) {
                merged.extend(cfg.mcp_servers);
            }
        }
        if let Ok(cfg) = load_config_file(Path::new(".mcp.json")) {
            merged.extend(cfg.mcp_servers);
        }

        McpConfig {
            mcp_servers: merged,
        }
    }

    /// Start all configured servers. Failures are logged, not fatal.
    pub async fn start_all(config: &McpConfig) -> Result<Self> {
        let mut servers = Vec::new();

        for (name, server_config) in &config.mcp_servers {
            match Self::connect(name, server_config).await {
                Ok(server) => {
                    tracing::info!("MCP '{}': {} tools", name, server.tools.len());
                    servers.push(server);
                }
                Err(e) => {
                    tracing::warn!("MCP '{}' failed: {}", name, e);
                }
            }
        }

        Ok(Self { servers })
    }

    /// Call a tool by server name and tool name directly.
    pub async fn call_tool(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult> {
        let server = self
            .servers
            .iter()
            .find(|s| s.name == server_name)
            .ok_or_else(|| anyhow!("MCP server '{}' not connected", server_name))?;

        let result = server
            .service
            .call_tool(CallToolRequestParam {
                name: tool_name.to_string().into(),
                arguments,
            })
            .await?;

        Ok(result)
    }

    /// All tools across all connected servers.
    pub fn all_tools(&self) -> Vec<McpTool> {
        self.servers
            .iter()
            .flat_map(|s| {
                s.tools.iter().map(|t| McpTool {
                    server_name: s.name.clone(),
                    tool: t.clone(),
                })
            })
            .collect()
    }

    /// Build system prompt context listing available MCP tools.
    pub fn build_context(&self) -> Option<String> {
        let tools = self.all_tools();
        if tools.is_empty() {
            return None;
        }

        let mut ctx = String::from(
            "## MCP Tools\n\nUse McpToolCall to call these. Specify server, tool, and arguments as JSON.\n\n",
        );

        let mut by_server: HashMap<&str, Vec<&McpTool>> = HashMap::new();
        for tool in &tools {
            by_server.entry(&tool.server_name).or_default().push(tool);
        }

        for (server, server_tools) in &by_server {
            ctx.push_str(&format!(
                "### {} ({} tools)\n\n",
                server,
                server_tools.len()
            ));
            for t in server_tools {
                ctx.push_str(&format!("- **{}**", t.tool.name));
                if let Some(desc) = &t.tool.description {
                    // Truncate long descriptions in context
                    let short: &str = if desc.len() > 120 { &desc[..120] } else { desc };
                    ctx.push_str(&format!(": {}", short));
                }
                ctx.push('\n');
            }
            ctx.push('\n');
        }

        Some(ctx)
    }

    /// Graceful shutdown of all servers.
    pub async fn shutdown(self) {
        for server in self.servers {
            let _ = server.service.cancel().await;
        }
    }

    pub fn server_count(&self) -> usize {
        self.servers.len()
    }
    pub fn tool_count(&self) -> usize {
        self.servers.iter().map(|s| s.tools.len()).sum()
    }
    pub fn server_names(&self) -> Vec<&str> {
        self.servers.iter().map(|s| s.name.as_str()).collect()
    }

    // --- Private ---

    async fn connect(name: &str, config: &McpServerConfig) -> Result<McpServer> {
        let mut cmd = Command::new(&config.command);
        cmd.args(&config.args);
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        // Suppress stderr from MCP servers — they pollute output with
        // progress bars, model loading logs, HTTP requests etc.
        let (transport, _stderr) = TokioChildProcess::builder(cmd)
            .stderr(std::process::Stdio::null())
            .spawn()?;
        let service: McpService = ().serve(transport).await?;
        let tools = service.list_tools(Default::default()).await?.tools;

        Ok(McpServer {
            name: name.to_string(),
            service,
            tools,
        })
    }
}

fn load_config_file(path: &Path) -> Result<McpConfig> {
    let content = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

/// Format MCP tool result content for agent consumption.
pub fn format_tool_result(result: &CallToolResult) -> String {
    use rmcp::model::RawContent;

    let mut output = String::new();
    for content in &result.content {
        match &content.raw {
            RawContent::Text(text) => {
                output.push_str(&text.text);
                output.push('\n');
            }
            RawContent::Image(img) => {
                output.push_str(&format!("[Image: {} bytes]\n", img.data.len()));
            }
            RawContent::Audio(audio) => {
                output.push_str(&format!("[Audio: {} bytes]\n", audio.data.len()));
            }
            _ => {
                output.push_str("[Unsupported content type]\n");
            }
        }
    }
    if result.is_error.unwrap_or(false) {
        output = format!("Error: {}", output);
    }
    output
}
