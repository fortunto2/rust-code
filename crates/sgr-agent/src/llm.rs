//! Llm — provider-agnostic LLM client.
//!
//! Public API: `LlmConfig` + `Llm`. No provider-specific types leak.
//!
//! Backend selection (compile-time features + runtime model detection):
//! - `oxide` feature + OpenAI model → openai-oxide (Responses API, fastest)
//! - `genai` feature → genai crate (multi-provider: OpenAI, Gemini, Anthropic, etc.)
//! - Both enabled → oxide for OpenAI models, genai for everything else
//!
//! ```no_run
//! use sgr_agent::{Llm, LlmConfig};
//!
//! let llm = Llm::new(&LlmConfig::auto("gpt-5.4"));          // → oxide (if feature enabled)
//! let llm = Llm::new(&LlmConfig::auto("gemini-2.0-flash")); // → genai
//! let llm = Llm::new(&LlmConfig::endpoint("sk-or-...", "https://openrouter.ai/api/v1", "gpt-4o")); // → genai (custom endpoint)
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
    #[cfg(feature = "genai")]
    Genai(crate::genai_client::GenaiClient),
    #[cfg(feature = "oxide")]
    Oxide(crate::oxide_client::OxideClient),
}

/// Provider-agnostic LLM client. Construct via `Llm::new(&LlmConfig)`.
pub struct Llm {
    inner: Backend,
}

impl Llm {
    /// Create from config. Backend auto-selected:
    /// - oxide for all models (Responses API works with OpenAI + OpenRouter + compatible)
    /// - genai only for Vertex AI (project_id) or when oxide feature is disabled
    pub fn new(config: &LlmConfig) -> Self {
        #[cfg(feature = "oxide")]
        {
            // Vertex AI needs genai (gcloud ADC auth)
            if config.project_id.is_none() {
                if let Ok(client) = crate::oxide_client::OxideClient::from_config(config) {
                    tracing::debug!(model = %config.model, backend = "oxide", "Llm backend selected");
                    return Self {
                        inner: Backend::Oxide(client),
                    };
                }
            }
        }

        #[cfg(feature = "genai")]
        {
            tracing::debug!(model = %config.model, backend = "genai", "Llm backend selected");
            return Self {
                inner: Backend::Genai(crate::genai_client::GenaiClient::from_config(config)),
            };
        }

        #[cfg(not(any(feature = "genai", feature = "oxide")))]
        {
            compile_error!("At least one of 'genai' or 'oxide' features must be enabled for Llm");
        }
    }

    /// Get a reference to the inner LlmClient.
    fn client(&self) -> &dyn LlmClient {
        match &self.inner {
            #[cfg(feature = "genai")]
            Backend::Genai(c) => c,
            #[cfg(feature = "oxide")]
            Backend::Oxide(c) => c,
        }
    }

    /// Upgrade to WebSocket mode for lower latency (oxide backend only).
    /// No-op for genai or when oxide-ws feature is not enabled.
    pub async fn connect_ws(&self) -> Result<(), SgrError> {
        #[cfg(feature = "oxide-ws")]
        if let Backend::Oxide(c) = &self.inner {
            return c.connect_ws().await;
        }
        Ok(())
    }

    /// Stream text completion, calling `on_token` for each chunk.
    /// Returns the full concatenated text.
    /// Note: only available with genai backend (oxide streaming not yet wired).
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
            #[cfg(feature = "oxide")]
            Backend::Oxide(_) => {
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

    /// Function calling with stateful session support (OpenAI Responses API).
    /// Returns tool calls + response_id for use as `previous_response_id` in next call.
    pub async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        match &self.inner {
            #[cfg(feature = "genai")]
            Backend::Genai(c) => {
                c.tools_call_stateful(messages, tools, previous_response_id)
                    .await
            }
            #[cfg(feature = "oxide")]
            Backend::Oxide(c) => {
                c.tools_call_stateful(messages, tools, previous_response_id)
                    .await
            }
        }
    }

    /// Structured output — generates JSON schema from `T`, sends via native response_format,
    /// parses result into `T`.
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
            #[cfg(feature = "genai")]
            Backend::Genai(_) => "genai",
            #[cfg(feature = "oxide")]
            Backend::Oxide(_) => "oxide",
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

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        self.client().complete(messages).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_from_auto_config() {
        let config = LlmConfig::auto("gpt-5.4");
        let llm = Llm::new(&config);
        // With oxide feature: "oxide", without: "genai"
        let name = llm.backend_name();
        assert!(name == "oxide" || name == "genai");
    }

    #[test]
    fn llm_gemini_uses_genai() {
        let config = LlmConfig::auto("gemini-2.0-flash");
        let llm = Llm::new(&config);
        assert_eq!(llm.backend_name(), "genai");
    }

    #[test]
    fn llm_custom_endpoint_uses_oxide() {
        // Custom base_url → oxide (OpenRouter supports Responses API)
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
        assert!(config.base_url.is_none());
        assert_eq!(config.temp, 0.7);
        assert!(config.max_tokens.is_none());
    }
}
