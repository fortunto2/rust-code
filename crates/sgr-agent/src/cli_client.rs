//! CliClient — LlmClient backed by CLI subprocess (claude/gemini/codex).
//!
//! Calls `claude -p "prompt"` and parses the text response using
//! flexible_parser to extract structured tool calls. Uses the CLI's
//! own auth (subscription credits), no API key needed.
//!
//! This enables using Claude Pro/Max subscription as a full agent backend
//! with tool calls, by putting tool schemas in the prompt and parsing
//! the text response back into `ToolCall` structs.

use crate::client::{LlmClient, synthesize_finish_if_empty};
use crate::tool::ToolDef;
use crate::types::{Message, Role, SgrError, ToolCall};
use crate::union_schema;
use serde_json::Value;
use std::process::Stdio;
use tokio::io::AsyncReadExt;

/// Which CLI binary to invoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliBackend {
    /// `claude -p` — Claude Code CLI (uses subscription).
    Claude,
    /// `gemini -p` — Gemini CLI.
    Gemini,
    /// `codex exec` — Codex CLI.
    Codex,
}

impl CliBackend {
    /// Detect from model name: "claude-cli" → Claude, etc.
    pub fn from_model(model: &str) -> Option<Self> {
        match model {
            "claude-cli" => Some(Self::Claude),
            "gemini-cli" => Some(Self::Gemini),
            "codex-cli" => Some(Self::Codex),
            _ => None,
        }
    }

    fn binary(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Gemini => "gemini",
            Self::Codex => "codex",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::Claude => "Claude CLI (subscription)",
            Self::Gemini => "Gemini CLI",
            Self::Codex => "Codex CLI",
        }
    }
}

/// LLM client that delegates to a CLI subprocess.
///
/// The CLI handles its own auth — no API keys needed.
/// Tool calls are emulated: tool schemas go into the prompt as text,
/// the CLI returns plain text, and we parse it back into `ToolCall`s.
#[derive(Debug, Clone)]
pub struct CliClient {
    backend: CliBackend,
    /// Model to pass via --model flag (e.g. "claude-sonnet-4-6").
    /// None = use CLI's default model.
    model: Option<String>,
}

impl CliClient {
    pub fn new(backend: CliBackend) -> Self {
        Self {
            backend,
            model: None,
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        let m = model.into();
        // Don't pass "claude-cli" as --model to the CLI
        if CliBackend::from_model(&m).is_none() {
            self.model = Some(m);
        }
        self
    }

    /// Flatten messages into a single prompt string for CLI stdin.
    fn flatten_messages(messages: &[Message]) -> String {
        let mut parts = Vec::with_capacity(messages.len());
        for msg in messages {
            if msg.content.is_empty() {
                continue;
            }
            let prefix = match msg.role {
                Role::System => "System",
                Role::User => "Human",
                Role::Assistant => "Assistant",
                Role::Tool => "Tool Result",
            };
            parts.push(format!("[{}]\n{}", prefix, msg.content));
        }
        parts.join("\n\n")
    }

    /// Build CLI command args for a prompt.
    fn build_args(&self, prompt: &str) -> (String, Vec<String>) {
        match self.backend {
            CliBackend::Claude => {
                let mut args = vec![
                    "-p".into(),
                    prompt.into(),
                    "--output-format".into(),
                    "text".into(),
                    "--no-session-persistence".into(),
                    "--max-turns".into(),
                    "1".into(),
                    // Disable Claude's own tools — we handle tool execution
                    "--disallowed-tools".into(),
                    "Bash,Edit,Write,Read,Glob,Grep,Agent".into(),
                ];
                if let Some(ref model) = self.model {
                    args.push("--model".into());
                    args.push(model.clone());
                }
                ("claude".into(), args)
            }
            CliBackend::Gemini => {
                let mut args = vec![
                    "-p".into(),
                    prompt.into(),
                    "--sandbox".into(),
                    "--output-format".into(),
                    "text".into(),
                ];
                if let Some(ref model) = self.model {
                    args.push("--model".into());
                    args.push(model.clone());
                }
                ("gemini".into(), args)
            }
            CliBackend::Codex => ("codex".into(), vec!["exec".into(), prompt.into()]),
        }
    }

    /// Run CLI subprocess and return output text.
    async fn run(&self, prompt: &str) -> Result<String, SgrError> {
        let (cmd, args) = self.build_args(prompt);

        let mut command = tokio::process::Command::new(&cmd);
        command
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Force subscription billing, not API billing
        if self.backend == CliBackend::Claude {
            command.env("CLAUDECODE", "");
            command.env_remove("ANTHROPIC_API_KEY");
        }

        let mut child = command.spawn().map_err(|e| SgrError::Api {
            status: 0,
            body: format!("{} not found: {}. Is it installed?", cmd, e),
        })?;

        let mut output = String::new();
        if let Some(mut out) = child.stdout.take() {
            out.read_to_string(&mut output)
                .await
                .map_err(|e| SgrError::Api {
                    status: 0,
                    body: e.to_string(),
                })?;
        }

        let mut err_output = String::new();
        if let Some(mut err) = child.stderr.take() {
            err.read_to_string(&mut err_output)
                .await
                .map_err(|e| SgrError::Api {
                    status: 0,
                    body: e.to_string(),
                })?;
        }

        let status = child.wait().await.map_err(|e| SgrError::Api {
            status: 0,
            body: e.to_string(),
        })?;

        if !status.success() && output.trim().is_empty() {
            return Err(SgrError::Api {
                status: status.code().unwrap_or(1) as u16,
                body: format!("{} failed: {}", cmd, err_output.trim()),
            });
        }

        let text = output.trim().to_string();
        tracing::info!(
            backend = self.backend.binary(),
            model = self.model.as_deref().unwrap_or("default"),
            output_chars = text.len(),
            "cli_client.complete"
        );

        Ok(text)
    }

    /// Build tool descriptions for text-based tool calling.
    fn tools_prompt(tools: &[ToolDef]) -> String {
        use crate::schema_simplifier;
        let mut s = String::from(
            "## Available Tools\n\n\
             You MUST respond with ONLY valid JSON (no markdown, no explanation):\n\
             {\"situation\": \"what you observe\", \"task\": [\"next steps\"], \
             \"actions\": [{\"tool_name\": \"<name>\", ...args}]}\n\n",
        );
        for t in tools {
            s.push_str(&schema_simplifier::simplify_tool(
                &t.name,
                &t.description,
                &t.parameters,
            ));
            s.push_str("\n\n");
        }
        s
    }
}

#[async_trait::async_trait]
impl LlmClient for CliClient {
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
        let schema_hint = format!(
            "\n\nRespond with ONLY valid JSON matching this schema:\n{}\n\
             No markdown, no explanations, no code blocks. Raw JSON only.",
            serde_json::to_string_pretty(schema).unwrap_or_default()
        );

        let mut prompt = Self::flatten_messages(messages);
        prompt.push_str(&schema_hint);

        let raw = self.run(&prompt).await?;
        let parsed = crate::flexible_parser::parse_flexible::<Value>(&raw)
            .map(|r| r.value)
            .ok();
        Ok((parsed, vec![], raw))
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        let tools_desc = Self::tools_prompt(tools);
        let mut prompt = Self::flatten_messages(messages);
        prompt.push_str("\n\n");
        prompt.push_str(&tools_desc);

        let raw = self.run(&prompt).await?;

        match union_schema::parse_action(&raw, tools) {
            Ok((_situation, mut calls)) => {
                synthesize_finish_if_empty(&mut calls, &raw);
                Ok(calls)
            }
            Err(e) => {
                tracing::warn!(error = %e, "CLI response parse failed, synthesizing finish");
                Ok(vec![ToolCall {
                    id: "cli_finish".into(),
                    name: "finish".into(),
                    arguments: serde_json::json!({"summary": raw}),
                }])
            }
        }
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        let prompt = Self::flatten_messages(messages);
        self.run(&prompt).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_messages_basic() {
        let msgs = vec![
            Message::system("You are helpful."),
            Message::user("Hello"),
            Message::assistant("Hi!"),
        ];
        let flat = CliClient::flatten_messages(&msgs);
        assert!(flat.contains("[System]"));
        assert!(flat.contains("[Human]"));
        assert!(flat.contains("[Assistant]"));
        assert!(flat.contains("You are helpful."));
    }

    #[test]
    fn flatten_skips_empty() {
        let msgs = vec![Message::system(""), Message::user("test")];
        let flat = CliClient::flatten_messages(&msgs);
        assert!(!flat.contains("[System]"));
        assert!(flat.contains("test"));
    }

    #[test]
    fn tools_prompt_contains_schema() {
        let tools = vec![ToolDef {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path"}
                },
                "required": ["path"]
            }),
        }];
        let prompt = CliClient::tools_prompt(&tools);
        assert!(prompt.contains("read_file"));
        assert!(prompt.contains("File path"));
        assert!(prompt.contains("tool_name"));
    }

    #[test]
    fn backend_from_model() {
        assert_eq!(
            CliBackend::from_model("claude-cli"),
            Some(CliBackend::Claude)
        );
        assert_eq!(
            CliBackend::from_model("gemini-cli"),
            Some(CliBackend::Gemini)
        );
        assert_eq!(CliBackend::from_model("gpt-4o"), None);
    }

    #[test]
    fn with_model_skips_cli_names() {
        let client = CliClient::new(CliBackend::Claude).with_model("claude-cli");
        assert!(client.model.is_none()); // "claude-cli" not passed as --model

        let client2 = CliClient::new(CliBackend::Claude).with_model("claude-sonnet-4-6");
        assert_eq!(client2.model.as_deref(), Some("claude-sonnet-4-6"));
    }
}
