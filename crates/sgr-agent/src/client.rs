//! LlmClient trait — abstract LLM backend for agent use.
//!
//! Implementations wrap `GeminiClient` / `OpenAIClient` existing methods.

use crate::tool::ToolDef;
use crate::types::{Message, SgrError, ToolCall};
use serde_json::Value;

/// Abstract LLM client for agent framework.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Structured call: send messages + schema, get parsed output + tool calls.
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

#[cfg(feature = "gemini")]
mod gemini_impl {
    use super::*;
    use crate::gemini::GeminiClient;

    #[async_trait::async_trait]
    impl LlmClient for GeminiClient {
        async fn structured_call(
            &self,
            messages: &[Message],
            _schema: &Value,
        ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
            // Use flexible mode which injects schema into system prompt
            let resp = self.flexible::<Value>(messages).await?;
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
            _schema: &Value,
        ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
            let resp = self.flexible::<Value>(messages).await?;
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
