//! SGR backend — pure Rust HTTP LLM provider.
//!
//! Uses native Gemini function calling (functionDeclarations) for Gemini/Vertex,
//! and flexible JSON parsing for OpenAI-compatible and CLI backends.

use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::client::LlmClient;
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
// Provider — wraps LlmConfig for unified LLM access
// ---------------------------------------------------------------------------

/// LLM provider wrapping `LlmConfig`. Provider is auto-detected from model name
/// by the genai crate. Auth comes from env vars or explicit api_key in config.
#[derive(Debug, Clone)]
pub struct LlmProvider {
    pub config: sgr_agent::LlmConfig,
}

impl LlmProvider {
    pub fn new(config: sgr_agent::LlmConfig) -> Self {
        Self { config }
    }

    /// Human-readable label for TUI/logs.
    pub fn label(&self) -> String {
        self.config.label()
    }

    /// Model name.
    pub fn model(&self) -> &str {
        &self.config.model
    }
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
    /// Call an API endpoint via OpenAPI spec.
    #[serde(rename = "api")]
    Api {
        action: String,
        #[serde(default)]
        api_name: Option<String>,
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        endpoint: Option<String>,
        /// Comma-separated key=value pairs: "owner=foo,repo=bar"
        #[serde(default)]
        params: Option<String>,
        /// JSON string body for POST/PUT/PATCH
        #[serde(default)]
        body: Option<String>,
    },
    #[serde(rename = "delegate_task")]
    DelegateTask {
        agent: String,
        task: String,
        #[serde(default)]
        cwd: Option<String>,
    },
    #[serde(rename = "delegate_status")]
    DelegateStatus {
        #[serde(default)]
        id: Option<String>,
    },
    #[serde(rename = "delegate_result")]
    DelegateResult { id: String },
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

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct ApiParams {
    /// Action: "load" (load an API spec), "search" (find endpoints), "call" (execute endpoint), "list" (show loaded APIs)
    action: String,
    /// API name (e.g. "github", "stripe", "cloudflare"). Required for load/search/call.
    #[serde(default)]
    api_name: Option<String>,
    /// Search query for "search" action (e.g. "create issue")
    #[serde(default)]
    query: Option<String>,
    /// Endpoint name for "call" action (e.g. "repos_owner_repo_issues_post")
    #[serde(default)]
    endpoint: Option<String>,
    /// Parameters for "call" action as comma-separated key=value pairs (e.g. "owner=foo,repo=bar,state=open")
    #[serde(default)]
    params: Option<String>,
    /// Request body JSON string for "call" action (POST/PUT/PATCH), e.g. "{\"title\": \"Bug\"}"
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct DelegateTaskParams {
    /// CLI agent to delegate to: "claude", "gemini", or "codex".
    agent: String,
    /// Task description for the delegate agent.
    task: String,
    /// Working directory (default: current cwd).
    #[serde(default)]
    cwd: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct DelegateStatusParams {
    /// Delegate ID to check. If omitted, shows all delegates.
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct DelegateResultParams {
    /// Delegate ID to get results from.
    id: String,
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
        tool::<ApiParams>(
            "api",
            "Call any REST API via OpenAPI spec. Actions: 'load' (api_name: github/stripe/cloudflare/...), 'search' (api_name + query), 'call' (api_name + endpoint + params + body), 'list' (show loaded APIs). Load an API first, search for the endpoint, then call it.",
        ),
        tool::<DelegateTaskParams>(
            "delegate_task",
            "Delegate a complex task to a powerful CLI agent (claude/gemini/codex). \
             Runs as a full autonomous agent in tmux background. Returns delegate ID immediately. \
             Use delegate_status to check progress, delegate_result to get output when done.",
        ),
        tool::<DelegateStatusParams>(
            "delegate_status",
            "Check status of delegated tasks (running/done). Omit id to see all.",
        ),
        tool::<DelegateResultParams>(
            "delegate_result",
            "Get the output from a completed delegate.",
        ),
    ]
}

// ---------------------------------------------------------------------------
// Conversion: SgrNextStep → BAML types
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// SGR backend: call LLM and parse response
// ---------------------------------------------------------------------------

impl LlmProvider {
    /// Call LLM with native function calling via genai.
    /// All providers (Gemini, OpenAI, Anthropic, Vertex) go through the same path.
    pub async fn call_flexible(&self, messages: &[sgr_agent::Message]) -> Result<SgrNextStep> {
        static TOOLS: std::sync::LazyLock<Vec<sgr_agent::tool::ToolDef>> =
            std::sync::LazyLock::new(sgr_tool_defs);
        let tools = &*TOOLS;

        // Replace JSON-only system prompt with function calling prompt.
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

        let llm = self.make_llm_client();
        let tool_calls = llm
            .tools_call(&messages, &tools)
            .await
            .map_err(|e| anyhow::anyhow!("LLM error: {}", e))?;

        let actions: Vec<SgrAction> = tool_calls
            .iter()
            .filter_map(tool_call_to_sgr_action)
            .collect();

        if actions.is_empty() {
            return Ok(SgrNextStep {
                situation: "Model completed without explicit tool call.".into(),
                task: vec![],
                actions: vec![SgrAction::Finish {
                    summary: "Task completed.".into(),
                }],
            });
        }

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
            "LLM function calling"
        );

        Ok(SgrNextStep {
            situation,
            task: vec![],
            actions,
        })
    }

    /// Create an LlmClient from this provider's config.
    pub fn make_llm_client(&self) -> sgr_agent::Llm {
        let mut cfg = self.config.clone();
        if cfg.max_tokens.is_none() {
            cfg.max_tokens = Some(4096);
        }
        sgr_agent::Llm::new(&cfg)
    }

    /// Create a fast/cheap LlmClient for context compaction (summarization).
    pub fn make_compaction_client(&self) -> Box<dyn sgr_agent::client::LlmClient> {
        Box::new(sgr_agent::Llm::new(&self.config.for_compaction()))
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
        "api" => Some(SgrAction::Api {
            action: s("action"),
            api_name: s_opt("api_name"),
            query: s_opt("query"),
            endpoint: s_opt("endpoint"),
            params: s_opt("params"),
            body: s_opt("body"),
        }),
        "delegate_task" => Some(SgrAction::DelegateTask {
            agent: s("agent"),
            task: s("task"),
            cwd: s_opt("cwd"),
        }),
        "delegate_status" => Some(SgrAction::DelegateStatus { id: s_opt("id") }),
        "delegate_result" => Some(SgrAction::DelegateResult { id: s("id") }),
        _ => {
            tracing::warn!(tool = %tc.name, "unknown native function call, skipping");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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
                images: vec![],
            }
        })
        .collect()
}

/// Convert Msg structs to sgr-agent Messages (preserves images).
pub fn msgs_to_sgr_messages(messages: &[crate::agent::Msg]) -> Vec<sgr_agent::Message> {
    messages
        .iter()
        .map(|m| {
            let r = match m.role.as_str() {
                "system" => sgr_agent::Role::System,
                "assistant" => sgr_agent::Role::Assistant,
                "tool" => sgr_agent::Role::Tool,
                _ => sgr_agent::Role::User,
            };
            sgr_agent::Message {
                role: r,
                content: m.content.clone(),
                tool_call_id: None,
                tool_calls: vec![],
                images: m.images.clone(),
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
