//! LlmClient trait — abstract LLM backend for agent use.
//!
//! Implementations wrap `GeminiClient` / `OpenAIClient` existing methods.
//! `structured_call` injects the schema into the system prompt for flexible parsing.

use crate::tool::ToolDef;
use crate::types::{Message, Role, SgrError, ToolCall};
use serde_json::Value;

/// Abstract LLM client for agent framework.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Structured call: send messages with schema injected into system prompt.
    /// Returns (parsed_output, native_tool_calls, raw_text).
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError>;

    /// Native function calling: send messages + tool defs, get tool calls.
    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError>;

    /// Plain text completion (no schema, no tools).
    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError>;
}

/// When a model responds with text content instead of tool calls,
/// synthesize a "finish" tool call so the agent loop gets the answer.
/// Call this in `tools_call` implementations after extracting tool calls.
pub fn synthesize_finish_if_empty(calls: &mut Vec<ToolCall>, content: &str) {
    if calls.is_empty() {
        let text = content.trim();
        if !text.is_empty() {
            calls.push(ToolCall {
                id: "synth_finish".into(),
                name: "finish".into(),
                arguments: serde_json::json!({"summary": text}),
            });
        }
    }
}

/// Inject schema into messages: append to existing system message or prepend a new one.
fn inject_schema(messages: &[Message], schema: &Value) -> Vec<Message> {
    let schema_hint = format!(
        "\n\nRespond with valid JSON matching this schema:\n{}\n\nDo NOT wrap in markdown code blocks. Output raw JSON only.",
        serde_json::to_string_pretty(schema).unwrap_or_default()
    );

    let mut msgs = Vec::with_capacity(messages.len() + 1);
    let mut injected = false;

    for msg in messages {
        if msg.role == Role::System && !injected {
            // Append schema to existing system message
            msgs.push(Message::system(format!("{}{}", msg.content, schema_hint)));
            injected = true;
        } else {
            msgs.push(msg.clone());
        }
    }

    if !injected {
        // No system message found — prepend one
        msgs.insert(0, Message::system(schema_hint));
    }

    msgs
}

#[cfg(feature = "gemini")]
mod gemini_impl {
    use super::*;
    use crate::gemini::GeminiClient;

    #[async_trait::async_trait]
    impl LlmClient for GeminiClient {
        async fn structured_call(
            &self,
            messages: &[Message],
            schema: &Value,
        ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
            let msgs = inject_schema(messages, schema);
            let resp = self.flexible::<Value>(&msgs).await?;
            Ok((resp.output, resp.tool_calls, resp.raw_text))
        }

        async fn tools_call(
            &self,
            messages: &[Message],
            tools: &[ToolDef],
        ) -> Result<Vec<ToolCall>, SgrError> {
            self.tools_call(messages, tools).await
        }

        async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
            let resp = self.flexible::<Value>(messages).await?;
            Ok(resp.raw_text)
        }
    }
}

#[cfg(feature = "openai")]
mod openai_impl {
    use super::*;
    use crate::openai::OpenAIClient;

    #[async_trait::async_trait]
    impl LlmClient for OpenAIClient {
        async fn structured_call(
            &self,
            messages: &[Message],
            schema: &Value,
        ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
            let msgs = inject_schema(messages, schema);
            let resp = self.flexible::<Value>(&msgs).await?;
            Ok((resp.output, resp.tool_calls, resp.raw_text))
        }

        async fn tools_call(
            &self,
            messages: &[Message],
            tools: &[ToolDef],
        ) -> Result<Vec<ToolCall>, SgrError> {
            self.tools_call(messages, tools).await
        }

        async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
            let resp = self.flexible::<Value>(messages).await?;
            Ok(resp.raw_text)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inject_schema_appends_to_existing_system() {
        let msgs = vec![
            Message::system("You are a coding agent."),
            Message::user("hello"),
        ];
        let schema = serde_json::json!({"type": "object"});
        let result = inject_schema(&msgs, &schema);

        assert_eq!(result.len(), 2);
        assert!(result[0].content.contains("You are a coding agent."));
        assert!(result[0].content.contains("Respond with valid JSON"));
        assert_eq!(result[0].role, Role::System);
    }

    #[test]
    fn inject_schema_prepends_when_no_system() {
        let msgs = vec![Message::user("hello")];
        let schema = serde_json::json!({"type": "object"});
        let result = inject_schema(&msgs, &schema);

        assert_eq!(result.len(), 2);
        assert_eq!(result[0].role, Role::System);
        assert!(result[0].content.contains("Respond with valid JSON"));
        assert_eq!(result[1].role, Role::User);
    }

    #[test]
    fn inject_schema_only_first_system_message() {
        let msgs = vec![
            Message::system("System 1"),
            Message::user("msg"),
            Message::system("System 2"),
        ];
        let schema = serde_json::json!({"type": "object"});
        let result = inject_schema(&msgs, &schema);

        assert_eq!(result.len(), 3);
        // First system gets schema
        assert!(result[0].content.contains("Respond with valid JSON"));
        // Second system unchanged
        assert_eq!(result[2].content, "System 2");
    }
}
