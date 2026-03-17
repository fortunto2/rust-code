//! Llm — provider-agnostic LLM client.
//!
//! Public API: `LlmConfig` + `Llm`. No provider-specific types leak.
//!
//! ```no_run
//! use sgr_agent::{Llm, LlmConfig};
//!
//! let llm = Llm::new(&LlmConfig::auto("gpt-4o"));
//! let llm = Llm::new(&LlmConfig::endpoint("sk-or-...", "https://openrouter.ai/api/v1", "gpt-4o"));
//! ```

use crate::client::LlmClient;
use crate::genai_client::GenaiClient;
use crate::schema::response_schema_for;
use crate::tool::ToolDef;
use crate::types::{LlmConfig, Message, SgrError, ToolCall};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Provider-agnostic LLM client. Construct via `Llm::new(&LlmConfig)`.
pub struct Llm {
    inner: GenaiClient,
}

impl Llm {
    /// Create from config. This is the single entry point.
    pub fn new(config: &LlmConfig) -> Self {
        Self {
            inner: GenaiClient::from_config(config),
        }
    }

    /// Stream text completion, calling `on_token` for each chunk.
    /// Returns the full concatenated text.
    pub async fn stream_complete<F>(
        &self,
        messages: &[Message],
        on_token: F,
    ) -> Result<String, SgrError>
    where
        F: FnMut(&str),
    {
        self.inner.stream_complete(messages, on_token).await
    }

    /// Non-streaming text completion.
    pub async fn generate(&self, messages: &[Message]) -> Result<String, SgrError> {
        self.inner.complete(messages).await
    }

    /// Function calling with stateful session support (OpenAI Responses API).
    /// Returns tool calls + response_id for use as `previous_response_id` in next call.
    pub async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        self.inner
            .tools_call_stateful(messages, tools, previous_response_id)
            .await
    }

    /// Structured output — generates JSON schema from `T`, sends via native response_format,
    /// parses result into `T`. Uses genai's JsonSpec (handles additionalProperties for OpenAI).
    pub async fn structured<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
    ) -> Result<T, SgrError> {
        let schema = response_schema_for::<T>();
        let (parsed, _tool_calls, raw_text) = self.inner.structured_call(messages, &schema).await?;
        match parsed {
            Some(value) => serde_json::from_value::<T>(value)
                .map_err(|e| SgrError::Schema(format!("Parse error: {e}\nRaw: {raw_text}"))),
            None => Err(SgrError::EmptyResponse),
        }
    }
}

#[async_trait::async_trait]
impl LlmClient for Llm {
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
        self.inner.structured_call(messages, schema).await
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        self.inner.tools_call(messages, tools).await
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        self.inner.complete(messages).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_from_auto_config() {
        let config = LlmConfig::auto("gpt-4o");
        let llm = Llm::new(&config);
        assert_eq!(llm.inner.model, "gpt-4o");
    }

    #[test]
    fn llm_from_endpoint_config() {
        let config = LlmConfig::endpoint("sk-test", "https://api.example.com/v1", "my-model")
            .temperature(0.5)
            .max_tokens(2048);
        let llm = Llm::new(&config);
        assert_eq!(llm.inner.model, "my-model");
        assert_eq!(llm.inner.temperature, Some(0.5));
        assert_eq!(llm.inner.max_tokens, Some(2048));
    }

    #[test]
    fn llm_from_key_config() {
        let config = LlmConfig::with_key("sk-test", "claude-3-haiku");
        let llm = Llm::new(&config);
        assert_eq!(llm.inner.model, "claude-3-haiku");
    }

    #[test]
    fn llm_config_serde_roundtrip() {
        let config = LlmConfig::endpoint("key", "https://example.com/v1", "model")
            .temperature(0.9)
            .max_tokens(1000);
        let json = serde_json::to_string(&config).unwrap();
        let back: LlmConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.model, "model");
        assert_eq!(back.api_key.as_deref(), Some("key"));
        assert_eq!(back.base_url.as_deref(), Some("https://example.com/v1"));
        assert_eq!(back.temp, 0.9);
        assert_eq!(back.max_tokens, Some(1000));
    }

    #[test]
    fn llm_config_auto_minimal_json() {
        let json = r#"{"model": "gpt-4o"}"#;
        let config: LlmConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.model, "gpt-4o");
        assert!(config.api_key.is_none());
        assert!(config.base_url.is_none());
        assert_eq!(config.temp, 0.7); // default
        assert!(config.max_tokens.is_none());
    }
}
