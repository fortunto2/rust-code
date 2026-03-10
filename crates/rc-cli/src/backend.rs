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
        };

        // If structured parsing succeeded, return it
        if let Some(step) = resp.output {
            return Ok(step);
        }

        // Text fallback: model responded with prose instead of JSON.
        // Wrap in a finish action so the agent loop completes gracefully.
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
