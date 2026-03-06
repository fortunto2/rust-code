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

// --- Config types matching .mcp.json format ---

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

// --- MCP tool with server name prefix ---

#[derive(Debug, Clone)]
pub struct McpTool {
    pub server_name: String,
    pub tool: Tool,
}

impl McpTool {
    /// Prefixed name like "server__tool_name"
    pub fn prefixed_name(&self) -> String {
        format!("mcp__{}_{}", self.server_name, self.tool.name)
    }
}

// --- Running MCP server handle ---

type McpService = RunningService<rmcp::RoleClient, ()>;

struct McpServer {
    name: String,
    service: McpService,
    tools: Vec<Tool>,
}

// --- MCP Manager ---

pub struct McpManager {
    servers: Vec<McpServer>,
}

impl McpManager {
    pub fn new() -> Self {
        Self {
            servers: Vec::new(),
        }
    }

    /// Load .mcp.json configs from project dir and home dir, merge them.
    pub fn load_configs() -> McpConfig {
        let mut merged = HashMap::new();

        // Global: ~/.mcp.json
        if let Some(home) = dirs_path() {
            let global = home.join(".mcp.json");
            if let Ok(cfg) = load_config_file(&global) {
                merged.extend(cfg.mcp_servers);
            }
        }

        // Project: ./.mcp.json
        let project = PathBuf::from(".mcp.json");
        if let Ok(cfg) = load_config_file(&project) {
            merged.extend(cfg.mcp_servers);
        }

        McpConfig { mcp_servers: merged }
    }

    /// Start all configured MCP servers and collect their tools.
    pub async fn start_all(config: &McpConfig) -> Result<Self> {
        let mut manager = Self::new();

        for (name, server_config) in &config.mcp_servers {
            match start_server(name, server_config).await {
                Ok(server) => {
                    tracing::info!("MCP server '{}' started with {} tools", name, server.tools.len());
                    manager.servers.push(server);
                }
                Err(e) => {
                    tracing::warn!("Failed to start MCP server '{}': {}", name, e);
                }
            }
        }

        Ok(manager)
    }

    /// Start a single MCP server by name.
    pub async fn start_one(&mut self, name: &str, config: &McpServerConfig) -> Result<()> {
        let server = start_server(name, config).await?;
        // Remove existing with same name
        self.servers.retain(|s| s.name != name);
        self.servers.push(server);
        Ok(())
    }

    /// Get all tools from all connected servers.
    pub fn all_tools(&self) -> Vec<McpTool> {
        let mut tools = Vec::new();
        for server in &self.servers {
            for tool in &server.tools {
                tools.push(McpTool {
                    server_name: server.name.clone(),
                    tool: tool.clone(),
                });
            }
        }
        tools
    }

    /// Call a tool by prefixed name (mcp__server__tool).
    pub async fn call_tool(
        &self,
        prefixed_name: &str,
        arguments: Option<serde_json::Map<String, serde_json::Value>>,
    ) -> Result<CallToolResult> {
        let (server_name, tool_name) = parse_prefixed_name(prefixed_name)?;

        let server = self.servers.iter()
            .find(|s| s.name == server_name)
            .ok_or_else(|| anyhow!("MCP server '{}' not found", server_name))?;

        let result = server.service.call_tool(CallToolRequestParam {
            name: tool_name.into(),
            arguments,
        }).await?;

        Ok(result)
    }

    /// Build context string for agent system prompt listing available MCP tools.
    pub fn build_context(&self) -> Option<String> {
        let tools = self.all_tools();
        if tools.is_empty() {
            return None;
        }

        let mut ctx = String::from("## MCP Tools\n\nThe following MCP server tools are available. Use BashCommandTool to call them or request them by name.\n\n");

        let mut by_server: HashMap<&str, Vec<&McpTool>> = HashMap::new();
        for tool in &tools {
            by_server.entry(&tool.server_name).or_default().push(tool);
        }

        for (server, server_tools) in &by_server {
            ctx.push_str(&format!("### {} ({} tools)\n\n", server, server_tools.len()));
            for t in server_tools {
                ctx.push_str(&format!("- **{}**", t.tool.name));
                if let Some(desc) = &t.tool.description {
                    ctx.push_str(&format!(": {}", desc));
                }
                ctx.push('\n');
            }
            ctx.push('\n');
        }

        Some(ctx)
    }

    /// Shut down all servers.
    pub async fn shutdown(self) {
        for server in self.servers {
            let _ = server.service.cancel().await;
        }
    }

    /// Number of connected servers.
    pub fn server_count(&self) -> usize {
        self.servers.len()
    }

    /// Total number of tools across all servers.
    pub fn tool_count(&self) -> usize {
        self.servers.iter().map(|s| s.tools.len()).sum()
    }

    /// List server names.
    pub fn server_names(&self) -> Vec<&str> {
        self.servers.iter().map(|s| s.name.as_str()).collect()
    }
}

// --- Helpers ---

async fn start_server(name: &str, config: &McpServerConfig) -> Result<McpServer> {
    let mut cmd = Command::new(&config.command);
    cmd.args(&config.args);

    // Set environment variables
    for (key, value) in &config.env {
        cmd.env(key, value);
    }

    let transport = TokioChildProcess::new(cmd)?;
    let service: McpService = ().serve(transport).await?;

    // List available tools
    let tools_response = service.list_tools(Default::default()).await?;

    Ok(McpServer {
        name: name.to_string(),
        service,
        tools: tools_response.tools,
    })
}

fn load_config_file(path: &Path) -> Result<McpConfig> {
    let content = std::fs::read_to_string(path)?;
    let config: McpConfig = serde_json::from_str(&content)?;
    Ok(config)
}

fn dirs_path() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// Parse "mcp__server__tool_name" into ("server", "tool_name").
fn parse_prefixed_name(name: &str) -> Result<(String, String)> {
    let name = name.strip_prefix("mcp__").unwrap_or(name);
    // Split on first underscore after server name
    // Format: server_name + "__" + tool_name  (but we used single _ in prefixed_name)
    // Actually our format is mcp__{server}_{tool}
    // We need to find the server by matching against known servers
    // Simpler: split on first _
    if let Some(pos) = name.find('_') {
        let server = &name[..pos];
        let tool = &name[pos + 1..];
        if !server.is_empty() && !tool.is_empty() {
            return Ok((server.to_string(), tool.to_string()));
        }
    }
    Err(anyhow!("Invalid MCP tool name format: {}", name))
}

/// Format CallToolResult content as a string for the agent.
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
                output.push_str("[Unknown content type]\n");
            }
        }
    }
    if result.is_error.unwrap_or(false) {
        output = format!("Error: {}", output);
    }
    output
}
