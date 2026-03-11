//! SGR backend — pure Rust HTTP LLM provider.
//!
//! Uses native Gemini function calling (functionDeclarations) for Gemini/Vertex,
//! and flexible JSON parsing for OpenAI-compatible and CLI backends.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::tool::tool;

/// System prompt for native function calling mode (Gemini/Vertex).
/// Tells model to use provided functions instead of writing JSON.
const NATIVE_FC_SYSTEM_PROMPT: &str = r#"You are rust-code, an expert AI coding agent in a Terminal UI.

## How to respond
1. Think about the current situation — what phase of the task are you in? What's done, what's blocking?
2. Call one or more tools using the provided functions. Call multiple tools for independent operations.
3. When the task is FULLY complete, call the `finish` function with a summary of what was done.

## Editing files
Use `apply_patch` for ALL file edits. Do NOT use `edit_file` — it is deprecated.

The `apply_patch` tool uses a patch format:
```
*** Begin Patch
*** Update File: path/to/file
@@ optional_context_line
 context line (space prefix = unchanged)
-line to remove
+line to add
 context line
*** End Patch
```
- `*** Add File: path` — create new file (all lines start with +)
- `*** Delete File: path` — delete file
- `*** Update File: path` — edit existing file with hunks
- Each hunk starts with `@@` optionally followed by a function/class name for disambiguation
- Show 3 lines of context before and after changes
- Use space prefix for unchanged context lines, `-` for removals, `+` for additions
- File paths are relative to working directory

## Rules
- Act directly — don't waste steps on unnecessary setup.
- Read files before editing. Verify changes with git_status or tests.
- When answering questions or reporting findings, call `finish` with the answer in `summary`.
- For simple questions that need no tools, just call `finish` immediately.
- Keep going until the task is completely done. Don't stop at the first error.
"#;

// ---------------------------------------------------------------------------
// Provider enum
// ---------------------------------------------------------------------------

/// LLM provider configuration.
#[derive(Debug, Clone)]
pub enum SgrProvider {
    Gemini {
        api_key: String,
        model: String,
    },
    OpenAI {
        api_key: String,
        model: String,
        base_url: Option<String>,
    },
    /// Vertex AI — uses gcloud ADC (Application Default Credentials).
    /// No API key needed, uses `gcloud auth application-default print-access-token`.
    Vertex {
        project_id: String,
        model: String,
        location: String,
    },
    /// Gemini CLI subprocess — direct `gemini -p` call, read stdout.
    /// Uses CLI subscription (no API key needed).
    GeminiCli {
        model: Option<String>,
        sandbox: bool,
    },
}

// ---------------------------------------------------------------------------
// Serde-compatible mirror types for sgr-agent flexible parser
// ---------------------------------------------------------------------------
// These use #[serde(tag = "tool_name")] which is more reliable than
// BAML's untagged union for LLM text parsing.

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SgrNextStep {
    pub situation: String,
    pub task: Vec<String>,
    pub actions: Vec<SgrAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "tool_name")]
pub enum SgrAction {
    #[serde(rename = "read_file")]
    ReadFile {
        path: String,
        #[serde(default)]
        offset: Option<i64>,
        #[serde(default)]
        limit: Option<i64>,
    },
    #[serde(rename = "write_file")]
    WriteFile { path: String, content: String },
    #[serde(rename = "edit_file")]
    EditFile {
        path: String,
        old_string: String,
        new_string: String,
    },
    #[serde(rename = "bash")]
    Bash {
        command: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        timeout: Option<i64>,
    },
    #[serde(rename = "bash_bg")]
    BashBg { name: String, command: String },
    #[serde(rename = "search_code")]
    SearchCode { query: String },
    #[serde(rename = "git_status")]
    GitStatus {
        #[serde(default)]
        dummy: Option<String>,
    },
    #[serde(rename = "git_diff")]
    GitDiff {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        cached: Option<bool>,
    },
    #[serde(rename = "git_add")]
    GitAdd { paths: Vec<String> },
    #[serde(rename = "git_commit")]
    GitCommit { message: String },
    #[serde(rename = "open_editor")]
    OpenEditor {
        path: String,
        #[serde(default)]
        line: Option<i64>,
    },
    #[serde(rename = "ask_user")]
    AskUser { question: String },
    #[serde(rename = "finish")]
    Finish { summary: String },
    #[serde(rename = "mcp_call")]
    McpCall {
        server: String,
        tool: String,
        #[serde(default)]
        arguments: Option<String>,
    },
    #[serde(rename = "memory")]
    Memory {
        operation: String,
        #[serde(default)]
        category: Option<String>,
        #[serde(default)]
        section: Option<String>,
        #[serde(default)]
        content: Option<String>,
        #[serde(default)]
        context: Option<String>,
        #[serde(default)]
        confidence: Option<String>,
    },
    #[serde(rename = "project_map")]
    ProjectMap {
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "dependencies")]
    Dependencies {
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "task")]
    Task {
        operation: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        task_id: Option<i64>,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        priority: Option<String>,
        #[serde(default)]
        notes: Option<String>,
    },
    #[serde(rename = "apply_patch")]
    ApplyPatch { patch: String },
    #[serde(rename = "spawn_agent")]
    SpawnAgent {
        role: String,
        task: String,
        #[serde(default)]
        max_steps: Option<i64>,
    },
    #[serde(rename = "wait_agents")]
    WaitAgents {
        #[serde(default)]
        agent_ids: Vec<String>,
        #[serde(default)]
        timeout_secs: Option<i64>,
    },
    #[serde(rename = "agent_status")]
    AgentStatus {
        #[serde(default)]
        agent_id: Option<String>,
    },
    #[serde(rename = "cancel_agent")]
    CancelAgent { agent_id: String },
}

// ---------------------------------------------------------------------------
// SgrPlan — structured output for native function calling
// ---------------------------------------------------------------------------
// When using Gemini/Vertex with functionDeclarations, the model returns:
// - JSON text (SgrPlan): situation + task (the "thinking" part)
// - functionCall parts: the actual tool invocations
// We merge them into SgrNextStep.

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SgrPlan {
    /// Current situation assessment.
    pub situation: String,
    /// Task steps to accomplish.
    pub task: Vec<String>,
}

// ---------------------------------------------------------------------------
// Tool parameter structs for functionDeclarations
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ReadFileParams {
    /// File path to read.
    path: String,
    /// Line offset to start reading from.
    #[serde(default)]
    offset: Option<i64>,
    /// Number of lines to read.
    #[serde(default)]
    limit: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct WriteFileParams {
    /// File path to write.
    path: String,
    /// File content.
    content: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct EditFileParams {
    /// File path to edit.
    path: String,
    /// Exact string to find and replace.
    old_string: String,
    /// Replacement string.
    new_string: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct BashParams {
    /// Shell command to execute.
    command: String,
    /// Human-readable description of what this command does.
    #[serde(default)]
    description: Option<String>,
    /// Timeout in milliseconds.
    #[serde(default)]
    timeout: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct BashBgParams {
    /// Background task name.
    name: String,
    /// Shell command to run in background.
    command: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SearchCodeParams {
    /// Search query (regex or text).
    query: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GitStatusParams {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GitDiffParams {
    /// Optional path to diff.
    #[serde(default)]
    path: Option<String>,
    /// Show staged changes only.
    #[serde(default)]
    cached: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GitAddParams {
    /// File paths to stage.
    paths: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct GitCommitParams {
    /// Commit message.
    message: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct OpenEditorParams {
    /// File path to open.
    path: String,
    /// Line number to jump to.
    #[serde(default)]
    line: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct AskUserParams {
    /// Question to ask the user.
    question: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct FinishParams {
    /// Summary of what was accomplished.
    summary: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct McpCallParams {
    /// MCP server name.
    server: String,
    /// Tool name on the server.
    tool: String,
    /// JSON-encoded arguments.
    #[serde(default)]
    arguments: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct MemoryParams {
    /// Operation: "save" or "forget".
    operation: String,
    /// Category: insight, pattern, decision, preference, debug.
    #[serde(default)]
    category: Option<String>,
    /// Section name.
    #[serde(default)]
    section: Option<String>,
    /// Memory content.
    #[serde(default)]
    content: Option<String>,
    /// Context for this memory.
    #[serde(default)]
    context: Option<String>,
    /// Confidence: "confirmed" or "tentative".
    #[serde(default)]
    confidence: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ProjectMapParams {
    /// Optional path to scope the map.
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct DependenciesParams {
    /// Optional path to check dependencies.
    #[serde(default)]
    path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct TaskParams {
    /// Operation: create, list, update, done.
    operation: String,
    /// Task title (for create).
    #[serde(default)]
    title: Option<String>,
    /// Task ID (for update/done).
    #[serde(default)]
    task_id: Option<i64>,
    /// Status: todo, in_progress, blocked, done.
    #[serde(default)]
    status: Option<String>,
    /// Priority: low, medium, high.
    #[serde(default)]
    priority: Option<String>,
    /// Notes.
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ApplyPatchParams {
    /// Patch in the apply_patch format. Must start with "*** Begin Patch" and end with "*** End Patch".
    patch: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct SpawnAgentParams {
    /// Role: "explorer" (fast, read-only), "worker" (smart, read-write), "reviewer" (read-only, thorough).
    role: String,
    /// Task description for the sub-agent.
    task: String,
    /// Optional max steps before auto-stop.
    #[serde(default)]
    max_steps: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct WaitAgentsParams {
    /// Agent IDs to wait for. Empty = wait for all.
    #[serde(default)]
    agent_ids: Vec<String>,
    /// Timeout in seconds (default: 300).
    #[serde(default)]
    timeout_secs: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct AgentStatusParams {
    /// Agent ID to check. If omitted, shows all agents.
    #[serde(default)]
    agent_id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CancelAgentParams {
    /// Agent ID to cancel. Use "all" to cancel all agents.
    agent_id: String,
}

/// Build Gemini functionDeclarations for all agent tools.
pub fn sgr_tool_defs() -> Vec<sgr_agent::tool::ToolDef> {
    vec![
        tool::<ReadFileParams>(
            "read_file",
            "Read file contents. Use offset/limit for large files.",
        ),
        tool::<WriteFileParams>("write_file", "Create or overwrite a file with new content."),
        tool::<EditFileParams>(
            "edit_file",
            "DEPRECATED — use apply_patch instead. Simple single-string replacement (old_string → new_string).",
        ),
        tool::<ApplyPatchParams>(
            "apply_patch",
            "Edit files. Use this for ALL file modifications. Format: '*** Begin Patch\\n*** Update File: path\\n@@ optional_context\\n context_line\\n-old_line\\n+new_line\\n*** End Patch'. Operations: Add/Delete/Update File. Lines prefixed with space (context), - (remove), + (add). Include 3 lines of context around changes.",
        ),
        tool::<BashParams>("bash", "Run a shell command and return stdout/stderr."),
        tool::<BashBgParams>("bash_bg", "Run a shell command in background (tmux)."),
        tool::<SearchCodeParams>(
            "search_code",
            "Search codebase for a pattern using ripgrep.",
        ),
        tool::<GitStatusParams>("git_status", "Show git status of the working directory."),
        tool::<GitDiffParams>(
            "git_diff",
            "Show git diff. Use cached=true for staged changes.",
        ),
        tool::<GitAddParams>("git_add", "Stage files for commit."),
        tool::<GitCommitParams>("git_commit", "Create a git commit with a message."),
        tool::<OpenEditorParams>("open_editor", "Open a file in the user's editor."),
        tool::<AskUserParams>(
            "ask_user",
            "Ask the user a question and wait for their response.",
        ),
        tool::<FinishParams>(
            "finish",
            "Signal task completion with a summary of what was done.",
        ),
        tool::<McpCallParams>("mcp_call", "Call a tool on an MCP server."),
        tool::<MemoryParams>("memory", "Save or forget an agent memory entry."),
        tool::<ProjectMapParams>("project_map", "Generate a project structure map."),
        tool::<DependenciesParams>("dependencies", "Analyze project dependencies."),
        tool::<TaskParams>("task", "Manage tasks: create, list, update, done."),
        tool::<SpawnAgentParams>(
            "spawn_agent",
            "Spawn a sub-agent with a role and task. Roles: explorer (fast, read-only), worker (smart, read-write), reviewer (thorough, read-only). Returns agent ID.",
        ),
        tool::<WaitAgentsParams>(
            "wait_agents",
            "Wait for sub-agents to complete. Returns their results.",
        ),
        tool::<AgentStatusParams>(
            "agent_status",
            "Check status of sub-agents (running/completed/failed).",
        ),
        tool::<CancelAgentParams>(
            "cancel_agent",
            "Cancel a running sub-agent by ID, or 'all' to cancel all.",
        ),
    ]
}

// ---------------------------------------------------------------------------
// Conversion: SgrNextStep → BAML types
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// SGR backend: call LLM and parse response
// ---------------------------------------------------------------------------

impl SgrProvider {
    /// Call LLM and parse into SgrNextStep.
    /// Gemini/Vertex use native function calling (structured plan + tool calls).
    /// OpenAI/GeminiCli use flexible text parsing (legacy).
    pub async fn call_flexible(&self, messages: &[sgr_agent::Message]) -> Result<SgrNextStep> {
        match self {
            SgrProvider::Gemini { .. } | SgrProvider::Vertex { .. } => {
                self.call_native(messages).await
            }
            SgrProvider::OpenAI { .. } | SgrProvider::GeminiCli { .. } => {
                self.call_flexible_legacy(messages).await
            }
        }
    }

    /// Native function calling with functionDeclarations.
    /// Model returns text (situation analysis) + functionCall parts (tool invocations).
    async fn call_native(&self, messages: &[sgr_agent::Message]) -> Result<SgrNextStep> {
        let tools = sgr_tool_defs();

        // Replace JSON-only system prompt with function calling prompt.
        // The original prompt tells model to respond with JSON, but with native
        // function calling the model should write text analysis + call functions.
        let messages: Vec<sgr_agent::Message> = messages
            .iter()
            .map(|m| {
                if m.role == sgr_agent::Role::System
                    && m.content.contains("MUST respond with ONLY valid JSON")
                {
                    sgr_agent::Message::system(NATIVE_FC_SYSTEM_PROMPT)
                } else {
                    m.clone()
                }
            })
            .collect();

        let client = self.make_gemini_client().await?;
        let tool_calls = client
            .tools_call(&messages, &tools)
            .await
            .map_err(|e| anyhow::anyhow!("SGR native FC error: {}", e))?;

        let actions: Vec<SgrAction> = tool_calls
            .iter()
            .filter_map(tool_call_to_sgr_action)
            .collect();

        if actions.is_empty() {
            return Err(anyhow::anyhow!("SGR: model returned no tool calls"));
        }

        // Extract situation from a finish tool if present, otherwise generic
        let situation = actions
            .iter()
            .find_map(|a| {
                if let SgrAction::Finish { summary } = a {
                    Some(summary.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| format!("Executing {} tool(s).", actions.len()));

        tracing::info!(
            n = actions.len(),
            situation = situation.as_str(),
            "SGR native function calling"
        );

        Ok(SgrNextStep {
            situation,
            task: vec![],
            actions,
        })
    }

    /// Create a GeminiClient for this provider.
    pub async fn make_gemini_client(&self) -> Result<sgr_agent::gemini::GeminiClient> {
        match self {
            SgrProvider::Gemini { api_key, model } => {
                let mut config = sgr_agent::ProviderConfig::gemini(api_key, model);
                config.max_tokens = Some(4096);
                Ok(sgr_agent::gemini::GeminiClient::new(config))
            }
            SgrProvider::Vertex {
                project_id,
                model,
                location,
            } => {
                let access_token = get_gcloud_access_token().await?;
                let mut config =
                    sgr_agent::ProviderConfig::vertex(&access_token, project_id, model);
                config.location = Some(location.clone());
                config.max_tokens = Some(4096);
                Ok(sgr_agent::gemini::GeminiClient::new(config))
            }
            _ => Err(anyhow::anyhow!(
                "make_gemini_client: not a Gemini/Vertex provider"
            )),
        }
    }

    /// Create a fast/cheap LlmClient for context compaction (summarization).
    /// Uses Flash Lite model to keep costs low.
    pub async fn make_compaction_client(&self) -> Result<Box<dyn sgr_agent::client::LlmClient>> {
        match self {
            SgrProvider::Gemini { api_key, .. } => {
                let mut config =
                    sgr_agent::ProviderConfig::gemini(api_key, "gemini-2.0-flash-lite");
                config.max_tokens = Some(2048);
                Ok(Box::new(sgr_agent::gemini::GeminiClient::new(config)))
            }
            SgrProvider::Vertex {
                project_id,
                location,
                ..
            } => {
                let access_token = get_gcloud_access_token().await?;
                let mut config = sgr_agent::ProviderConfig::vertex(
                    &access_token,
                    project_id,
                    "gemini-2.0-flash-lite",
                );
                config.location = Some(location.clone());
                config.max_tokens = Some(2048);
                Ok(Box::new(sgr_agent::gemini::GeminiClient::new(config)))
            }
            SgrProvider::OpenAI {
                api_key, base_url, ..
            } => {
                let mut config = sgr_agent::ProviderConfig::openai(api_key, "gpt-4o-mini");
                if let Some(url) = base_url {
                    config.base_url = Some(url.clone());
                }
                config.max_tokens = Some(2048);
                Ok(Box::new(sgr_agent::openai::OpenAIClient::new(config)))
            }
            SgrProvider::GeminiCli { .. } => Err(anyhow::anyhow!(
                "Compaction not supported with GeminiCli provider"
            )),
        }
    }

    /// Legacy flexible text parsing for OpenAI/GeminiCli.
    async fn call_flexible_legacy(&self, messages: &[sgr_agent::Message]) -> Result<SgrNextStep> {
        let resp = match self {
            SgrProvider::OpenAI {
                api_key,
                model,
                base_url,
            } => {
                let mut config = sgr_agent::ProviderConfig::openai(api_key, model);
                config.base_url = base_url.clone();
                config.max_tokens = Some(4096);
                let client = sgr_agent::openai::OpenAIClient::new(config);
                client
                    .flexible::<SgrNextStep>(messages)
                    .await
                    .map_err(|e| anyhow::anyhow!("SGR OpenAI error: {}", e))?
            }
            SgrProvider::GeminiCli { model, sandbox } => {
                let raw_text = run_gemini_cli(messages, model.as_deref(), *sandbox).await?;
                let normalized = normalize_cli_json(&raw_text);
                let output =
                    sgr_agent::flexible_parser::parse_flexible_coerced::<SgrNextStep>(&normalized)
                        .map(|r| r.value)
                        .ok();
                sgr_agent::SgrResponse {
                    output,
                    tool_calls: vec![],
                    raw_text,
                    usage: None,
                    rate_limit: None,
                }
            }
            _ => unreachable!("call_flexible_legacy only for OpenAI/GeminiCli"),
        };

        // If structured parsing succeeded, return it
        if let Some(step) = resp.output {
            return Ok(step);
        }

        // Text fallback: model responded with prose instead of JSON.
        let text = resp.raw_text.trim();
        if !text.is_empty() {
            tracing::warn!("SGR text fallback: model returned prose, wrapping in finish");
            Ok(SgrNextStep {
                situation: "Model responded with text instead of structured JSON.".into(),
                task: vec!["Deliver the model's response to the user.".into()],
                actions: vec![SgrAction::Finish {
                    summary: text.to_string(),
                }],
            })
        } else {
            Err(anyhow::anyhow!("SGR: empty response from model"))
        }
    }
}

/// Convert a native Gemini function call to an SgrAction.
/// Maps tool_name → SgrAction variant, extracting args from JSON.
fn tool_call_to_sgr_action(tc: &sgr_agent::ToolCall) -> Option<SgrAction> {
    let args = &tc.arguments;
    let s = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let s_opt = |key: &str| args.get(key).and_then(|v| v.as_str()).map(String::from);
    let i_opt = |key: &str| args.get(key).and_then(|v| v.as_i64());

    match tc.name.as_str() {
        "read_file" => Some(SgrAction::ReadFile {
            path: s("path"),
            offset: i_opt("offset"),
            limit: i_opt("limit"),
        }),
        "write_file" => Some(SgrAction::WriteFile {
            path: s("path"),
            content: s("content"),
        }),
        "edit_file" => Some(SgrAction::EditFile {
            path: s("path"),
            old_string: s("old_string"),
            new_string: s("new_string"),
        }),
        "bash" => Some(SgrAction::Bash {
            command: s("command"),
            description: s_opt("description"),
            timeout: i_opt("timeout"),
        }),
        "search_code" => Some(SgrAction::SearchCode { query: s("query") }),
        "git_status" => Some(SgrAction::GitStatus { dummy: None }),
        "git_diff" => Some(SgrAction::GitDiff {
            path: s_opt("path"),
            cached: args.get("cached").and_then(|v| v.as_bool()),
        }),
        "git_add" => Some(SgrAction::GitAdd {
            paths: args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        }),
        "git_commit" => Some(SgrAction::GitCommit {
            message: s("message"),
        }),
        "finish" => Some(SgrAction::Finish {
            summary: s("summary"),
        }),
        "ask_user" => Some(SgrAction::AskUser {
            question: s("question"),
        }),
        "memory" => Some(SgrAction::Memory {
            operation: s("operation"),
            category: s_opt("category"),
            section: s_opt("section"),
            content: s_opt("content"),
            context: s_opt("context"),
            confidence: s_opt("confidence"),
        }),
        "mcp_call" => Some(SgrAction::McpCall {
            server: s("server"),
            tool: s("tool"),
            arguments: s_opt("arguments"),
        }),
        "bash_bg" => Some(SgrAction::BashBg {
            name: s("name"),
            command: s("command"),
        }),
        "open_editor" => Some(SgrAction::OpenEditor {
            path: s("path"),
            line: i_opt("line"),
        }),
        "project_map" => Some(SgrAction::ProjectMap {
            path: s_opt("path"),
        }),
        "dependencies" => Some(SgrAction::Dependencies {
            path: s_opt("path"),
        }),
        "task" => Some(SgrAction::Task {
            operation: s("operation"),
            title: s_opt("title"),
            task_id: i_opt("task_id"),
            status: s_opt("status"),
            priority: s_opt("priority"),
            notes: s_opt("notes"),
        }),
        "apply_patch" => Some(SgrAction::ApplyPatch { patch: s("patch") }),
        _ => {
            tracing::warn!(tool = %tc.name, "unknown native function call, skipping");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Gemini CLI subprocess
// ---------------------------------------------------------------------------

/// Get a fresh access token from gcloud.
/// Tries ADC first, falls back to user credentials.
/// Token is short-lived (~1h), so we get a new one per LLM call.
async fn get_gcloud_access_token() -> Result<String> {
    // Try ADC first
    let output = tokio::process::Command::new("gcloud")
        .args(["auth", "application-default", "print-access-token"])
        .output()
        .await;

    if let Ok(ref out) = output {
        if out.status.success() {
            let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !token.is_empty() {
                return Ok(token);
            }
        }
    }

    // Fall back to user credentials
    let output = tokio::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run gcloud: {}. Is it installed?", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "gcloud auth failed: {}. Run: gcloud auth login",
            stderr.trim()
        ));
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Err(anyhow::anyhow!(
            "Empty gcloud token. Run: gcloud auth login"
        ));
    }
    Ok(token)
}

/// Normalize LLM JSON output to match SgrNextStep schema.
/// Handles common deviations:
/// - "tool" → "tool_name"
/// - "parameters": {...} → flatten into action object
/// - "response"/"message"/"text"/"result" → "summary" (for finish tool)
fn normalize_cli_json(raw: &str) -> String {
    // Extract JSON from markdown blocks first
    let json_str = if let Some(start) = raw.find("```") {
        let after_ticks = &raw[start + 3..];
        // Skip optional language tag
        let content_start = after_ticks.find('\n').map(|i| i + 1).unwrap_or(0);
        let content = &after_ticks[content_start..];
        if let Some(end) = content.find("```") {
            content[..end].trim()
        } else {
            content.trim()
        }
    } else {
        raw.trim()
    };

    // Parse as JSON Value for manipulation
    let mut val: serde_json::Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(_) => return raw.to_string(), // can't parse, return original for flexible parser
    };

    // Ensure required top-level fields exist
    if !val.get("situation").is_some_and(|v| v.is_string()) {
        val["situation"] = serde_json::json!("Executing...");
    }
    if !val.get("task").is_some_and(|v| v.is_array()) {
        val["task"] = serde_json::json!(["Execute actions"]);
    }

    // Normalize actions array
    if let Some(actions) = val.get_mut("actions").and_then(|a| a.as_array_mut()) {
        for action in actions.iter_mut() {
            if let Some(obj) = action.as_object_mut() {
                // "tool" → "tool_name"
                if obj.contains_key("tool") && !obj.contains_key("tool_name") {
                    if let Some(tool_val) = obj.remove("tool") {
                        obj.insert("tool_name".into(), tool_val);
                    }
                }
                // Flatten "parameters" into action object
                if let Some(params) = obj.remove("parameters") {
                    if let Some(params_obj) = params.as_object() {
                        for (k, v) in params_obj {
                            if !obj.contains_key(k) {
                                obj.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
                // "file_path" → "path" (common LLM deviation)
                if obj.contains_key("file_path") && !obj.contains_key("path") {
                    if let Some(v) = obj.remove("file_path") {
                        obj.insert("path".into(), v);
                    }
                }
                // Normalize finish tool: "response"/"message"/"text"/"result" → "summary"
                if obj.get("tool_name").and_then(|v| v.as_str()) == Some("finish") {
                    if !obj.contains_key("summary") {
                        for alt in &["response", "message", "text", "result", "content"] {
                            if let Some(v) = obj.remove(*alt) {
                                obj.insert("summary".into(), v);
                                break;
                            }
                        }
                    }
                }
            }
        }
    }

    serde_json::to_string(&val).unwrap_or_else(|_| raw.to_string())
}

/// Run `gemini -p` subprocess and return raw stdout text.
async fn run_gemini_cli(
    messages: &[sgr_agent::Message],
    model: Option<&str>,
    sandbox: bool,
) -> Result<String> {
    use tokio::process::Command;

    // Merge messages into a single prompt (system + user + assistant history)
    let prompt = messages
        .iter()
        .map(|m| {
            let prefix = match m.role {
                sgr_agent::Role::System => "[System]",
                sgr_agent::Role::User => "[User]",
                sgr_agent::Role::Assistant => "[Assistant]",
                sgr_agent::Role::Tool => "[Tool Result]",
            };
            format!("{}\n{}", prefix, m.content)
        })
        .collect::<Vec<_>>()
        .join("\n\n");

    let mut cmd = Command::new("gemini");
    cmd.arg("-p").arg(&prompt);
    cmd.arg("--output-format").arg("json"); // structured envelope with response + stats
    cmd.arg("--yolo"); // auto-accept if tools present
    cmd.arg("-e").arg(""); // no extensions = pure LLM proxy (no tools)

    if let Some(m) = model {
        cmd.arg("-m").arg(m);
    }
    if sandbox {
        cmd.arg("--sandbox");
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    tracing::info!(
        model = model.unwrap_or("default"),
        sandbox,
        prompt_len = prompt.len(),
        "gemini_cli_start"
    );

    let output = cmd
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to run `gemini`: {}. Is it installed?", e))?;

    if !output.status.success() && output.stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("gemini CLI error: {}", stderr.trim()));
    }

    let text = String::from_utf8_lossy(&output.stdout).to_string();

    // Parse JSON envelope: {"session_id": "...", "response": "...", "stats": {...}}
    let response_text = if let Ok(envelope) = serde_json::from_str::<serde_json::Value>(&text) {
        if let Some(resp) = envelope.get("response").and_then(|r| r.as_str()) {
            // Log token stats if available
            if let Some(stats) = envelope.get("stats") {
                if let Some(tokens) = stats.pointer("/models/gemini-2.5-flash/tokens") {
                    tracing::info!(
                        input = tokens.get("input").and_then(|v| v.as_u64()).unwrap_or(0),
                        output = tokens
                            .get("candidates")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0),
                        "gemini_cli_tokens"
                    );
                }
            }
            resp.to_string()
        } else {
            text.clone()
        }
    } else {
        // Not JSON envelope — clean up text output
        text.lines()
            .filter(|l| {
                !l.contains("GOOGLE_API_KEY and GEMINI_API_KEY are set")
                    && !l.starts_with("Loading extension:")
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    };

    tracing::info!(output_len = response_text.len(), "gemini_cli_done");
    Ok(response_text)
}

/// Convert (role, content) pairs to sgr-agent Messages.
pub fn to_sgr_messages(history: &[(String, String)]) -> Vec<sgr_agent::Message> {
    history
        .iter()
        .map(|(role, content)| {
            let r = match role.as_str() {
                "system" => sgr_agent::Role::System,
                "assistant" => sgr_agent::Role::Assistant,
                "tool" => sgr_agent::Role::Tool,
                _ => sgr_agent::Role::User,
            };
            sgr_agent::Message {
                role: r,
                content: content.clone(),
                tool_call_id: None,
                tool_calls: vec![],
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sgr_action_deserializes_with_tag() {
        let raw = r#"{"tool_name":"read_file","path":"src/main.rs"}"#;
        let action: SgrAction = serde_json::from_str(raw).unwrap();
        match action {
            SgrAction::ReadFile { path, .. } => assert_eq!(path, "src/main.rs"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn sgr_next_step_parses_full() {
        let raw = json!({
            "situation": "reading code",
            "task": ["check main"],
            "actions": [
                {"tool_name": "read_file", "path": "src/main.rs"},
                {"tool_name": "bash", "command": "cargo test"}
            ]
        });
        let step: SgrNextStep = serde_json::from_value(raw).unwrap();
        assert_eq!(step.actions.len(), 2);
        assert_eq!(step.situation, "reading code");
    }

    #[test]
    fn flexible_parse_into_sgr_types() {
        let raw = r#"{"situation":"fixing bug","task":["edit file"],"actions":[{"tool_name":"edit_file","path":"lib.rs","old_string":"foo","new_string":"bar"}]}"#;
        let result = sgr_agent::parse_flexible_coerced::<SgrNextStep>(raw);
        assert!(result.is_ok());
        let step = result.unwrap().value;
        assert_eq!(step.actions.len(), 1);
        match &step.actions[0] {
            SgrAction::EditFile {
                path,
                old_string,
                new_string,
            } => {
                assert_eq!(path, "lib.rs");
                assert_eq!(old_string, "foo");
                assert_eq!(new_string, "bar");
            }
            _ => panic!("wrong variant"),
        }
    }
}
