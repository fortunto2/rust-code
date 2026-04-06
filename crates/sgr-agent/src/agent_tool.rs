//! Tool trait — the core abstraction for agent tools.
//!
//! Implement `Tool` for each capability you want to expose to the agent.
//! Arguments arrive as `serde_json::Value`; use `parse_args` helper for typed deserialization.

use crate::tool::ToolDef;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Modifier that a tool can return to change agent runtime behavior.
///
/// Inspired by Claude Code's contextModifier pattern — tools don't just return text,
/// they can instruct the runtime to adjust its behavior for subsequent steps.
/// Well-known key in `AgentContext.custom` for max_tokens override.
/// Agents can read this in `prepare_context` to adjust LLM max_tokens.
pub const MAX_TOKENS_OVERRIDE_KEY: &str = "_max_tokens_override";

#[derive(Debug, Clone, Default)]
pub struct ContextModifier {
    /// Inject a context message for the next step (sent as Role::User for provider compat).
    pub system_injection: Option<String>,
    /// Override max_tokens for subsequent model calls.
    /// Stored in `AgentContext.custom[MAX_TOKENS_OVERRIDE_KEY]` — agents read it
    /// in `prepare_context` and pass to LlmConfig.
    pub max_tokens_override: Option<u32>,
    /// Add extra context to AgentContext.custom (key → value).
    pub custom_context: Vec<(String, serde_json::Value)>,
    /// Adjust max_steps by this delta (positive = more steps allowed).
    pub max_steps_delta: Option<i32>,
}

impl ContextModifier {
    pub fn system(msg: impl Into<String>) -> Self {
        Self {
            system_injection: Some(msg.into()),
            ..Default::default()
        }
    }

    pub fn max_tokens(tokens: u32) -> Self {
        Self {
            max_tokens_override: Some(tokens),
            ..Default::default()
        }
    }

    pub fn custom(key: impl Into<String>, value: serde_json::Value) -> Self {
        Self {
            custom_context: vec![(key.into(), value)],
            ..Default::default()
        }
    }

    pub fn extra_steps(delta: i32) -> Self {
        Self {
            max_steps_delta: Some(delta),
            ..Default::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.system_injection.is_none()
            && self.max_tokens_override.is_none()
            && self.custom_context.is_empty()
            && self.max_steps_delta.is_none()
    }
}

/// Output from a tool execution.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    /// Human-readable result content.
    pub content: String,
    /// If true, the agent should stop (e.g. FinishTask tool).
    pub done: bool,
    /// If true, the loop should pause and wait for user input.
    /// Content contains the question to ask.
    pub waiting: bool,
    /// Optional modifier to adjust agent runtime behavior.
    pub modifier: Option<ContextModifier>,
}

impl ToolOutput {
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            done: false,
            waiting: false,
            modifier: None,
        }
    }

    pub fn done(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            done: true,
            waiting: false,
            modifier: None,
        }
    }

    /// Signal that the agent needs user input before continuing.
    /// The content is the question to present to the user.
    pub fn waiting(question: impl Into<String>) -> Self {
        Self {
            content: question.into(),
            done: false,
            waiting: true,
            modifier: None,
        }
    }

    /// Attach a context modifier to this output.
    /// The modifier will be applied to the agent context after tool execution.
    pub fn with_modifier(mut self, modifier: ContextModifier) -> Self {
        self.modifier = Some(modifier);
        self
    }
}

/// Errors from tool execution.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    Execution(String),
    #[error("invalid args: {0}")]
    InvalidArgs(String),
}

/// Parse JSON args into a typed struct. Use inside `Tool::execute`.
pub fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, ToolError> {
    serde_json::from_value(args.clone()).map_err(|e| ToolError::InvalidArgs(e.to_string()))
}

/// A tool that an agent can invoke.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (used as discriminator in LLM output).
    fn name(&self) -> &str;

    /// Human-readable description for the LLM.
    fn description(&self) -> &str;

    /// System tools are always visible (not subject to progressive discovery).
    fn is_system(&self) -> bool {
        false
    }

    /// Whether this tool only reads state (no side effects).
    /// Read-only tools can be executed in parallel.
    fn is_read_only(&self) -> bool {
        false
    }

    /// JSON Schema for the tool's parameters.
    fn parameters_schema(&self) -> Value;

    /// Execute the tool with JSON arguments.
    async fn execute(
        &self,
        args: Value,
        ctx: &mut super::context::AgentContext,
    ) -> Result<ToolOutput, ToolError>;

    /// Execute without mutable context access. Used for parallel execution of read-only tools.
    /// Gets read-only context ref for cache lookups (tool_cache, observations).
    /// Default implementation panics — override if is_read_only() returns true.
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &super::context::AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let _ = args;
        panic!("execute_readonly called on tool that doesn't implement it")
    }

    /// Convert to a `ToolDef` for LLM API submission.
    fn to_def(&self) -> ToolDef {
        ToolDef {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::AgentContext;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize)]
    struct EchoArgs {
        message: String,
    }

    struct EchoTool;

    #[async_trait::async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }
        fn description(&self) -> &str {
            "Echo a message back"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                },
                "required": ["message"]
            })
        }
        async fn execute(
            &self,
            args: Value,
            _ctx: &mut AgentContext,
        ) -> Result<ToolOutput, ToolError> {
            let a: EchoArgs = parse_args(&args)?;
            Ok(ToolOutput::text(a.message))
        }
    }

    #[test]
    fn parse_args_valid() {
        let args = serde_json::json!({"message": "hello"});
        let parsed: EchoArgs = parse_args(&args).unwrap();
        assert_eq!(parsed.message, "hello");
    }

    #[test]
    fn parse_args_invalid() {
        let args = serde_json::json!({"wrong_field": 42});
        let result = parse_args::<EchoArgs>(&args);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ToolError::InvalidArgs(_)));
    }

    #[test]
    fn tool_to_def() {
        let tool = EchoTool;
        let def = tool.to_def();
        assert_eq!(def.name, "echo");
        assert_eq!(def.description, "Echo a message back");
        assert!(def.parameters["properties"]["message"].is_object());
    }

    #[tokio::test]
    async fn tool_execute() {
        let tool = EchoTool;
        let mut ctx = AgentContext::new();
        let args = serde_json::json!({"message": "world"});
        let output = tool.execute(args, &mut ctx).await.unwrap();
        assert_eq!(output.content, "world");
        assert!(!output.done);
    }

    #[test]
    fn tool_output_done() {
        let out = ToolOutput::done("finished");
        assert!(out.done);
        assert_eq!(out.content, "finished");
    }
}
