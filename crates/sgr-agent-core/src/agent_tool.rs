//! Tool trait — the core abstraction for agent tools.

use crate::tool::ToolDef;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Modifier that a tool can return to change agent runtime behavior.
///
/// Attach to `ToolOutput` via `.with_modifier()`. The agent loop applies
/// these after tool execution — injecting system messages, adjusting token limits, etc.
#[derive(Debug, Clone, Default)]
pub struct ContextModifier {
    /// Inject a message into context for the next LLM call.
    pub system_injection: Option<String>,
    /// Override max_tokens for subsequent calls.
    pub max_tokens_override: Option<u32>,
    /// Add key-value pairs to `AgentContext.custom`.
    pub custom_context: Vec<(String, serde_json::Value)>,
    /// Adjust remaining max_steps (positive = more steps allowed).
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
///
/// Construct via `ToolOutput::text("result")`, `ToolOutput::done("finished")`,
/// or `ToolOutput::waiting("question for user")`.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    pub content: String,
    pub done: bool,
    pub waiting: bool,
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

    pub fn waiting(question: impl Into<String>) -> Self {
        Self {
            content: question.into(),
            done: false,
            waiting: true,
            modifier: None,
        }
    }

    pub fn with_modifier(mut self, modifier: ContextModifier) -> Self {
        self.modifier = Some(modifier);
        self
    }
}

/// Errors from tool execution.
#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    /// Tool execution failed (I/O, network, logic error).
    #[error("{0}")]
    Execution(String),
    /// Tool arguments failed to parse or validate.
    #[error("invalid args: {0}")]
    InvalidArgs(String),
    /// Permission denied (sandbox, policy, auth).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// Tool not found or not available.
    #[error("not found: {0}")]
    NotFound(String),
    /// Timeout exceeded.
    #[error("timeout: {0}")]
    Timeout(String),
}

impl ToolError {
    /// Create an execution error from any error type.
    pub fn exec(err: impl std::fmt::Display) -> Self {
        Self::Execution(err.to_string())
    }
}

/// Parse JSON args into a typed struct. Use inside `Tool::execute`.
///
/// ```rust,ignore
/// let args: MyArgs = parse_args(&args)?;
/// ```
pub fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, ToolError> {
    serde_json::from_value(args.clone()).map_err(|e| ToolError::InvalidArgs(e.to_string()))
}

/// A tool that an agent can invoke.
///
/// Implement this trait for each capability you want to expose to the LLM agent.
/// Tools are registered in a `ToolRegistry` and dispatched by the agent loop.
///
/// Read-only tools (`is_read_only() -> true`) can execute in parallel.
/// Write tools execute sequentially with exclusive `&mut AgentContext`.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    /// Unique tool name (used as discriminator in LLM function calling).
    fn name(&self) -> &str;
    /// Human-readable description shown to the LLM.
    fn description(&self) -> &str;

    /// System tools are always visible (not subject to progressive discovery).
    fn is_system(&self) -> bool {
        false
    }
    /// Read-only tools can execute in parallel via `execute_readonly`.
    fn is_read_only(&self) -> bool {
        false
    }

    /// JSON Schema for the tool's parameters (generated via `json_schema_for::<Args>()`).
    fn parameters_schema(&self) -> Value;

    async fn execute(
        &self,
        args: Value,
        ctx: &mut crate::context::AgentContext,
    ) -> Result<ToolOutput, ToolError>;

    /// Execute without mutable context (for parallel read-only dispatch).
    /// Default: delegates to `execute` with a cloned context. Override for true
    /// read-only tools to avoid the clone.
    async fn execute_readonly(
        &self,
        args: Value,
        ctx: &crate::context::AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let mut ctx_clone = ctx.clone();
        self.execute(args, &mut ctx_clone).await
    }

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
                "properties": { "message": { "type": "string" } },
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
        let result = parse_args::<EchoArgs>(&serde_json::json!({"wrong": 42}));
        assert!(matches!(result.unwrap_err(), ToolError::InvalidArgs(_)));
    }

    #[tokio::test]
    async fn tool_execute() {
        let tool = EchoTool;
        let mut ctx = AgentContext::new();
        let output = tool
            .execute(serde_json::json!({"message": "world"}), &mut ctx)
            .await
            .unwrap();
        assert_eq!(output.content, "world");
    }
}
