//! Llm — provider-agnostic LLM client.
//!
//! Public API: `LlmConfig` + `Llm`. No provider-specific types leak.
//!
//! Backend selection:
//! - oxide (openai-oxide): primary, Responses API, works with OpenAI + OpenRouter + compatible
//! - genai (optional): fallback for Vertex AI (project_id set)
//!
//! ```no_run
//! use sgr_agent::{Llm, LlmConfig};
//!
//! let llm = Llm::new(&LlmConfig::auto("gpt-5.4"));
//! let llm = Llm::new(&LlmConfig::endpoint("sk-or-...", "https://openrouter.ai/api/v1", "gpt-4o"));
//! ```

use crate::client::LlmClient;
use crate::schema::response_schema_for;
use crate::tool::ToolDef;
use crate::types::{LlmConfig, Message, SgrError, ToolCall};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Backend dispatch — resolved at construction time.
enum Backend {
    Oxide(crate::oxide_client::OxideClient),
    OxideChat(crate::oxide_chat_client::OxideChatClient),
    #[cfg(feature = "genai")]
    Genai(crate::genai_client::GenaiClient),
}

/// Provider-agnostic LLM client. Construct via `Llm::new(&LlmConfig)`.
pub struct Llm {
    inner: Backend,
}

impl Llm {
    /// Create from config. Backend auto-selected:
    /// - oxide for all models (primary)
    /// - genai only for Vertex AI (project_id set)
    pub fn new(config: &LlmConfig) -> Self {
        // Vertex AI needs genai (gcloud ADC auth)
        #[cfg(feature = "genai")]
        if config.project_id.is_some() {
            tracing::debug!(model = %config.model, backend = "genai", "Llm backend selected");
            return Self {
                inner: Backend::Genai(crate::genai_client::GenaiClient::from_config(config)),
            };
        }

        // Chat Completions mode for compat endpoints (Cloudflare, OpenRouter compat, etc.)
        if config.use_chat_api
            && let Ok(client) = crate::oxide_chat_client::OxideChatClient::from_config(config)
        {
            tracing::debug!(model = %config.model, backend = "oxide-chat", "Llm backend selected (Chat Completions)");
            return Self {
                inner: Backend::OxideChat(client),
            };
        }

        if let Ok(client) = crate::oxide_client::OxideClient::from_config(config) {
            tracing::debug!(model = %config.model, backend = "oxide", "Llm backend selected");
            Self {
                inner: Backend::Oxide(client),
            }
        } else {
            #[cfg(feature = "genai")]
            {
                tracing::debug!(model = %config.model, backend = "genai", "Llm backend selected (oxide fallback)");
                return Self {
                    inner: Backend::Genai(crate::genai_client::GenaiClient::from_config(config)),
                };
            }
            #[cfg(not(feature = "genai"))]
            panic!("OxideClient::from_config failed and genai feature not enabled");
        }
    }

    /// Get a reference to the inner LlmClient.
    fn client(&self) -> &dyn LlmClient {
        match &self.inner {
            Backend::Oxide(c) => c,
            Backend::OxideChat(c) => c,
            #[cfg(feature = "genai")]
            Backend::Genai(c) => c,
        }
    }

    /// Upgrade to WebSocket mode for lower latency (oxide backend only).
    /// No-op for genai.
    pub async fn connect_ws(&self) -> Result<(), SgrError> {
        #[cfg(feature = "oxide-ws")]
        if let Backend::Oxide(c) = &self.inner {
            return c.connect_ws().await;
        }
        Ok(())
    }

    /// Stream text completion, calling `on_token` for each chunk.
    pub async fn stream_complete<F>(
        &self,
        messages: &[Message],
        mut on_token: F,
    ) -> Result<String, SgrError>
    where
        F: FnMut(&str),
    {
        match &self.inner {
            #[cfg(feature = "genai")]
            Backend::Genai(c) => c.stream_complete(messages, on_token).await,
            Backend::Oxide(_) | Backend::OxideChat(_) => {
                // Oxide doesn't have streaming yet — generate full text,
                // then invoke on_token so callers (e.g. TTS, TUI) get the content.
                let text = self.generate(messages).await?;
                on_token(&text);
                Ok(text)
            }
        }
    }

    /// Non-streaming text completion.
    pub async fn generate(&self, messages: &[Message]) -> Result<String, SgrError> {
        self.client().complete(messages).await
    }

    /// Function calling with stateful session support (Responses API).
    /// Delegates to the trait method — each backend implements its own version.
    pub async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        self.client()
            .tools_call_stateful(messages, tools, previous_response_id)
            .await
    }

    /// Structured output — generates JSON schema from `T`, parses result.
    pub async fn structured<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
    ) -> Result<T, SgrError> {
        let schema = response_schema_for::<T>();
        let (parsed, _tool_calls, raw_text) =
            self.client().structured_call(messages, &schema).await?;
        match parsed {
            Some(value) => serde_json::from_value::<T>(value)
                .map_err(|e| SgrError::Schema(format!("Parse error: {e}\nRaw: {raw_text}"))),
            None => Err(SgrError::EmptyResponse),
        }
    }

    /// Which backend is active.
    pub fn backend_name(&self) -> &'static str {
        match &self.inner {
            Backend::Oxide(_) => "oxide",
            Backend::OxideChat(_) => "oxide-chat",
            #[cfg(feature = "genai")]
            Backend::Genai(_) => "genai",
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
        self.client().structured_call(messages, schema).await
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        self.client().tools_call(messages, tools).await
    }

    async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        self.client()
            .tools_call_stateful(messages, tools, previous_response_id)
            .await
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        self.client().complete(messages).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_from_auto_config() {
        // OxideClient::from_config needs an API key — use config-based key
        let config = LlmConfig::endpoint("sk-test-dummy", "https://api.openai.com/v1", "gpt-5.4");
        let llm = Llm::new(&config);
        assert_eq!(llm.backend_name(), "oxide");
    }

    #[test]
    fn llm_custom_endpoint_uses_oxide() {
        let config = LlmConfig::endpoint("sk-test", "https://openrouter.ai/api/v1", "gpt-5.4");
        let llm = Llm::new(&config);
        assert_eq!(llm.backend_name(), "oxide");
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
        assert_eq!(config.temp, 0.7);
    }
}
