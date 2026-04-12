//! Tool trait — the core abstraction for agent tools.

use crate::tool::ToolDef;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Modifier that a tool can return to change agent runtime behavior.
#[derive(Debug, Clone, Default)]
pub struct ContextModifier {
    pub system_injection: Option<String>,
    pub max_tokens_override: Option<u32>,
    pub custom_context: Vec<(String, serde_json::Value)>,
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
    #[error("{0}")]
    Execution(String),
    #[error("invalid args: {0}")]
    InvalidArgs(String),
}

/// Parse JSON args into a typed struct.
pub fn parse_args<T: DeserializeOwned>(args: &Value) -> Result<T, ToolError> {
    serde_json::from_value(args.clone()).map_err(|e| ToolError::InvalidArgs(e.to_string()))
}

/// A tool that an agent can invoke.
#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    fn is_system(&self) -> bool {
        false
    }
    fn is_read_only(&self) -> bool {
        false
    }

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
