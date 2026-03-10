//! Switchable LLM backend: BAML (dlopen runtime) vs SGR (pure Rust HTTP).
//!
//! Both backends produce the same BAML types (NextStep, Action union)
//! so the rest of the system (execute, loop detection, TUI) stays unchanged.

use crate::agent::Action;
use crate::baml_client::types;
use anyhow::Result;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Backend enum
// ---------------------------------------------------------------------------

/// Which LLM backend to use for `decide()` / `decide_stream()`.
#[derive(Debug, Clone)]
pub enum Backend {
    /// BAML runtime (dlopen). Supports streaming + structured output.
    /// Optional client name override (e.g. "OllamaDefault", "CodexProxy").
    Baml(Option<String>),

    /// SGR-agent pure Rust. Uses flexible parser (text → JSON cascade).
    /// For iOS/Android/WASM or when BAML runtime is unavailable.
    Sgr(SgrProvider),
}

impl Default for Backend {
    fn default() -> Self {
        Self::Baml(None)
    }
}

/// SGR provider configuration.
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
}

// ---------------------------------------------------------------------------
// Conversion: SgrNextStep → BAML types
// ---------------------------------------------------------------------------

impl SgrNextStep {
    /// Convert to BAML's NextStep (used by the rest of the system).
    pub fn into_baml(self) -> types::NextStep {
        types::NextStep {
            situation: self.situation,
            task: self.task,
            actions: self.actions.into_iter().map(|a| a.into_baml()).collect(),
        }
    }
}

impl SgrAction {
    fn into_baml(self) -> Action {
        match self {
            SgrAction::ReadFile {
                path,
                offset,
                limit,
            } => Action::ReadFileTool(types::ReadFileTool {
                tool_name: "read_file".into(),
                path,
                offset,
                limit,
            }),
            SgrAction::WriteFile { path, content } => {
                Action::WriteFileTool(types::WriteFileTool {
                    tool_name: "write_file".into(),
                    path,
                    content,
                })
            }
            SgrAction::EditFile {
                path,
                old_string,
                new_string,
            } => Action::EditFileTool(types::EditFileTool {
                tool_name: "edit_file".into(),
                path,
                old_string,
                new_string,
            }),
            SgrAction::Bash {
                command,
                description,
                timeout,
            } => Action::BashCommandTool(types::BashCommandTool {
                tool_name: "bash".into(),
                command,
                description,
                timeout,
            }),
            SgrAction::BashBg { name, command } => Action::BashBgTool(types::BashBgTool {
                tool_name: "bash_bg".into(),
                name,
                command,
            }),
            SgrAction::SearchCode { query } => {
                Action::SearchCodeTool(types::SearchCodeTool {
                    tool_name: "search_code".into(),
                    query,
                })
            }
            SgrAction::GitStatus { dummy } => Action::GitStatusTool(types::GitStatusTool {
                tool_name: "git_status".into(),
                dummy,
            }),
            SgrAction::GitDiff { path, cached } => Action::GitDiffTool(types::GitDiffTool {
                tool_name: "git_diff".into(),
                path,
                cached,
            }),
            SgrAction::GitAdd { paths } => Action::GitAddTool(types::GitAddTool {
                tool_name: "git_add".into(),
                paths,
            }),
            SgrAction::GitCommit { message } => {
                Action::GitCommitTool(types::GitCommitTool {
                    tool_name: "git_commit".into(),
                    message,
                })
            }
            SgrAction::OpenEditor { path, line } => {
                Action::OpenEditorTool(types::OpenEditorTool {
                    tool_name: "open_editor".into(),
                    path,
                    line,
                })
            }
            SgrAction::AskUser { question } => Action::AskUserTool(types::AskUserTool {
                tool_name: "ask_user".into(),
                question,
            }),
            SgrAction::Finish { summary } => {
                Action::FinishTaskTool(types::FinishTaskTool {
                    tool_name: "finish".into(),
                    summary,
                })
            }
            SgrAction::McpCall {
                server,
                tool,
                arguments,
            } => Action::McpToolCall(types::McpToolCall {
                tool_name: "mcp_call".into(),
                server,
                tool,
                arguments,
            }),
            SgrAction::Memory {
                operation,
                category,
                section,
                content,
                context,
                confidence,
            } => Action::MemoryTool(types::MemoryTool {
                tool_name: "memory".into(),
                operation: parse_memory_operation(&operation),
                category: parse_memory_category(category.as_deref().unwrap_or("insight")),
                section: section.unwrap_or_default(),
                content: content.unwrap_or_default(),
                context,
                confidence: parse_confidence(confidence.as_deref().unwrap_or("tentative")),
            }),
            SgrAction::ProjectMap { path } => {
                Action::ProjectMapTool(types::ProjectMapTool {
                    tool_name: "project_map".into(),
                    path,
                })
            }
            SgrAction::Dependencies { path } => {
                Action::DependenciesTool(types::DependenciesTool {
                    tool_name: "dependencies".into(),
                    path,
                })
            }
            SgrAction::Task {
                operation,
                title,
                task_id,
                status,
                priority,
                notes,
            } => Action::TaskTool(types::TaskTool {
                tool_name: "task".into(),
                operation: parse_task_operation(&operation),
                title,
                task_id,
                status: status.map(|s| parse_task_status(&s)),
                priority: priority.map(|p| parse_task_priority(&p)),
                notes,
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// BAML literal union helpers
// ---------------------------------------------------------------------------

use types::{
    Union2KconfirmedOrKtentative, Union2KforgetOrKsave,
    Union3KhighOrKlowOrKmedium, Union4KblockedOrKdoneOrKin_progressOrKtodo,
    Union4KcreateOrKdoneOrKlistOrKupdate,
    Union5KdebugOrKdecisionOrKinsightOrKpatternOrKpreference,
};

fn parse_memory_operation(s: &str) -> Union2KforgetOrKsave {
    match s.to_lowercase().as_str() {
        "forget" | "delete" | "remove" => Union2KforgetOrKsave::Kforget,
        _ => Union2KforgetOrKsave::Ksave,
    }
}

fn parse_memory_category(
    s: &str,
) -> Union5KdebugOrKdecisionOrKinsightOrKpatternOrKpreference {
    match s.to_lowercase().as_str() {
        "decision" => Union5KdebugOrKdecisionOrKinsightOrKpatternOrKpreference::Kdecision,
        "pattern" => Union5KdebugOrKdecisionOrKinsightOrKpatternOrKpreference::Kpattern,
        "preference" => Union5KdebugOrKdecisionOrKinsightOrKpatternOrKpreference::Kpreference,
        "debug" => Union5KdebugOrKdecisionOrKinsightOrKpatternOrKpreference::Kdebug,
        _ => Union5KdebugOrKdecisionOrKinsightOrKpatternOrKpreference::Kinsight,
    }
}

fn parse_confidence(s: &str) -> Union2KconfirmedOrKtentative {
    match s.to_lowercase().as_str() {
        "confirmed" => Union2KconfirmedOrKtentative::Kconfirmed,
        _ => Union2KconfirmedOrKtentative::Ktentative,
    }
}

fn parse_task_operation(s: &str) -> Union4KcreateOrKdoneOrKlistOrKupdate {
    match s.to_lowercase().as_str() {
        "list" => Union4KcreateOrKdoneOrKlistOrKupdate::Klist,
        "update" => Union4KcreateOrKdoneOrKlistOrKupdate::Kupdate,
        "done" => Union4KcreateOrKdoneOrKlistOrKupdate::Kdone,
        _ => Union4KcreateOrKdoneOrKlistOrKupdate::Kcreate,
    }
}

fn parse_task_status(s: &str) -> Union4KblockedOrKdoneOrKin_progressOrKtodo {
    match s.to_lowercase().as_str() {
        "in_progress" | "in-progress" | "inprogress" => {
            Union4KblockedOrKdoneOrKin_progressOrKtodo::Kin_progress
        }
        "blocked" => Union4KblockedOrKdoneOrKin_progressOrKtodo::Kblocked,
        "done" => Union4KblockedOrKdoneOrKin_progressOrKtodo::Kdone,
        _ => Union4KblockedOrKdoneOrKin_progressOrKtodo::Ktodo,
    }
}

fn parse_task_priority(s: &str) -> Union3KhighOrKlowOrKmedium {
    match s.to_lowercase().as_str() {
        "medium" | "med" => Union3KhighOrKlowOrKmedium::Kmedium,
        "high" | "critical" => Union3KhighOrKlowOrKmedium::Khigh,
        _ => Union3KhighOrKlowOrKmedium::Klow,
    }
}

// ---------------------------------------------------------------------------
// SGR backend: call LLM and parse response
// ---------------------------------------------------------------------------

impl SgrProvider {
    /// Call LLM in flexible mode and parse into SgrNextStep.
    /// If parsing fails but the model returned text, wraps it in a finish action
    /// (text fallback) so the agent loop doesn't crash.
    pub async fn call_flexible(
        &self,
        messages: &[sgr_agent::Message],
    ) -> Result<SgrNextStep> {
        let resp = match self {
            SgrProvider::Gemini { api_key, model } => {
                let mut config = sgr_agent::ProviderConfig::gemini(api_key, model);
                config.max_tokens = Some(4096);
                let client = sgr_agent::gemini::GeminiClient::new(config);
                client.flexible::<SgrNextStep>(messages).await
                    .map_err(|e| anyhow::anyhow!("SGR Gemini error: {}", e))?
            }
            SgrProvider::OpenAI {
                api_key,
                model,
                base_url,
            } => {
                let mut config = sgr_agent::ProviderConfig::openai(api_key, model);
                config.base_url = base_url.clone();
                config.max_tokens = Some(4096);
                let client = sgr_agent::openai::OpenAIClient::new(config);
                client.flexible::<SgrNextStep>(messages).await
                    .map_err(|e| anyhow::anyhow!("SGR OpenAI error: {}", e))?
            }
            SgrProvider::Vertex { project_id, model, location } => {
                let access_token = get_gcloud_access_token().await?;
                let mut config = sgr_agent::ProviderConfig::vertex(&access_token, project_id, model);
                config.location = Some(location.clone());
                config.max_tokens = Some(4096);
                let client = sgr_agent::gemini::GeminiClient::new(config);
                client.flexible::<SgrNextStep>(messages).await
                    .map_err(|e| anyhow::anyhow!("SGR Vertex error: {}", e))?
            }
            SgrProvider::GeminiCli { model, sandbox } => {
                let raw_text = run_gemini_cli(messages, model.as_deref(), *sandbox).await?;
                // Normalize CLI output: fix common LLM deviations before parsing
                let normalized = normalize_cli_json(&raw_text);
                let output = sgr_agent::flexible_parser::parse_flexible_coerced::<SgrNextStep>(&normalized)
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
        };

        // If structured parsing succeeded, return it
        if let Some(step) = resp.output {
            return Ok(step);
        }

        // Native function call fallback: model used Gemini functionCall parts
        // instead of text JSON. Convert tool_calls → SgrAction.
        if !resp.tool_calls.is_empty() {
            tracing::info!(
                n = resp.tool_calls.len(),
                "SGR native function call fallback"
            );
            let actions: Vec<SgrAction> = resp
                .tool_calls
                .iter()
                .filter_map(|tc| tool_call_to_sgr_action(tc))
                .collect();
            if !actions.is_empty() {
                return Ok(SgrNextStep {
                    situation: "Executing tool calls from native function calling.".into(),
                    task: vec!["Execute the requested actions.".into()],
                    actions,
                });
            }
        }

        // Text fallback: model responded with prose instead of JSON.
        // Wrap in a finish action so the agent loop doesn't crash.
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
    let s = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
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
                .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                .unwrap_or_default(),
        }),
        "git_commit" => Some(SgrAction::GitCommit { message: s("message") }),
        "finish" => Some(SgrAction::Finish { summary: s("summary") }),
        "ask_user" => Some(SgrAction::AskUser { question: s("question") }),
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
        return Err(anyhow::anyhow!("Empty gcloud token. Run: gcloud auth login"));
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
                        output = tokens.get("candidates").and_then(|v| v.as_u64()).unwrap_or(0),
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

/// Convert BAML Message history to sgr-agent Messages.
pub fn to_sgr_messages(history: &[types::Message]) -> Vec<sgr_agent::Message> {
    history
        .iter()
        .map(|m| {
            let role = match m.role.as_str() {
                "system" => sgr_agent::Role::System,
                "assistant" => sgr_agent::Role::Assistant,
                "tool" => sgr_agent::Role::Tool,
                _ => sgr_agent::Role::User,
            };
            sgr_agent::Message {
                role,
                content: m.content.clone(),
                tool_call_id: None,
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
    fn sgr_to_baml_conversion() {
        let sgr = SgrNextStep {
            situation: "test".into(),
            task: vec!["t1".into()],
            actions: vec![
                SgrAction::ReadFile {
                    path: "main.rs".into(),
                    offset: None,
                    limit: None,
                },
                SgrAction::Finish {
                    summary: "done".into(),
                },
            ],
        };
        let baml = sgr.into_baml();
        assert_eq!(baml.situation, "test");
        assert_eq!(baml.actions.len(), 2);
        match &baml.actions[0] {
            Action::ReadFileTool(r) => assert_eq!(r.path, "main.rs"),
            _ => panic!("wrong variant"),
        }
        match &baml.actions[1] {
            Action::FinishTaskTool(f) => assert_eq!(f.summary, "done"),
            _ => panic!("wrong variant"),
        }
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

    #[test]
    fn memory_tool_string_to_baml_enum() {
        assert!(matches!(
            parse_memory_operation("save"),
            Union2KforgetOrKsave::Ksave
        ));
        assert!(matches!(
            parse_memory_operation("forget"),
            Union2KforgetOrKsave::Kforget
        ));
        assert!(matches!(
            parse_memory_operation("SAVE"),
            Union2KforgetOrKsave::Ksave
        ));
    }

    #[test]
    fn task_operation_string_to_baml_enum() {
        assert!(matches!(
            parse_task_operation("create"),
            Union4KcreateOrKdoneOrKlistOrKupdate::Kcreate
        ));
        assert!(matches!(
            parse_task_operation("LIST"),
            Union4KcreateOrKdoneOrKlistOrKupdate::Klist
        ));
    }
}
