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
use crate::retry::RetryClient;
use crate::schema::response_schema_for;
use crate::tool::ToolDef;
use crate::types::{LlmConfig, Message, SgrError, ToolCall};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Backend dispatch — resolved at construction time.
/// All network backends wrapped in RetryClient for automatic retry on transient errors.
enum Backend {
    Oxide(RetryClient<crate::oxide_client::OxideClient>),
    OxideChat(RetryClient<crate::oxide_chat_client::OxideChatClient>),
    #[cfg(feature = "genai")]
    Genai(crate::genai_client::GenaiClient),
    /// CLI subprocess (claude -p / gemini -p / codex exec).
    Cli(crate::cli_client::CliClient),
    /// Construction failed — returns the captured error on every call.
    /// Lets `Llm::new` stay infallible while surfacing a clean error at call sites.
    /// Only reachable when the `genai` feature is off; with `genai` we always have a fallback.
    #[cfg_attr(feature = "genai", allow(dead_code))]
    Failed(String),
}

/// Provider-agnostic LLM client. Construct via `Llm::new(&LlmConfig)`.
pub struct Llm {
    inner: Backend,
}

impl Llm {
    /// Create from config. Backend auto-selected:
    /// - genai when explicitly requested (`use_genai`) or for Vertex AI (project_id set)
    /// - oxide-chat for Chat Completions compat endpoints
    /// - oxide for all other models (primary)
    pub fn new(config: &LlmConfig) -> Self {
        // CLI subprocess backend (claude -p / gemini -p / codex exec)
        if config.use_cli {
            let backend = crate::cli_client::CliBackend::from_model(&config.model)
                .unwrap_or(crate::cli_client::CliBackend::Claude);
            let client = crate::cli_client::CliClient::new(backend).with_model(&config.model);
            tracing::debug!(model = %config.model, backend = "cli", "Llm backend selected");
            return Self {
                inner: Backend::Cli(client),
            };
        }

        // Explicit genai backend (e.g. Anthropic native API)
        #[cfg(feature = "genai")]
        if config.use_genai {
            tracing::debug!(model = %config.model, backend = "genai", "Llm backend selected (explicit)");
            return Self {
                inner: Backend::Genai(crate::genai_client::GenaiClient::from_config(config)),
            };
        }

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
                inner: Backend::OxideChat(RetryClient::new(client)),
            };
        }

        match crate::oxide_client::OxideClient::from_config(config) {
            Ok(client) => {
                tracing::debug!(model = %config.model, backend = "oxide", "Llm backend selected");
                Self {
                    inner: Backend::Oxide(RetryClient::new(client)),
                }
            }
            Err(_e) => {
                #[cfg(feature = "genai")]
                {
                    tracing::debug!(model = %config.model, backend = "genai", "Llm backend selected (oxide fallback)");
                    Self {
                        inner: Backend::Genai(crate::genai_client::GenaiClient::from_config(
                            config,
                        )),
                    }
                }
                #[cfg(not(feature = "genai"))]
                {
                    let msg = format!(
                        "no LLM backend available for model `{}` — set `api_key` or `base_url` \
                         in ~/.rust-code/config.toml, or pick a known provider \
                         (gemini, openai, claude, ollama, mistralrs, lmstudio, vllm, cloudflare). \
                         Underlying error: {}",
                        config.model, _e
                    );
                    tracing::error!("{msg}");
                    Self {
                        inner: Backend::Failed(msg),
                    }
                }
            }
        }
    }

    /// Get a reference to the inner LlmClient.
    /// Returns `None` if construction failed — callers map that to `SgrError`.
    fn client(&self) -> Option<&dyn LlmClient> {
        match &self.inner {
            Backend::Oxide(c) => Some(c),
            Backend::OxideChat(c) => Some(c),
            #[cfg(feature = "genai")]
            Backend::Genai(c) => Some(c),
            Backend::Cli(c) => Some(c),
            Backend::Failed(_) => None,
        }
    }

    /// Captured construction error, if any.
    fn failure(&self) -> Option<SgrError> {
        match &self.inner {
            Backend::Failed(msg) => Some(SgrError::Schema(msg.clone())),
            _ => None,
        }
    }

    /// Create from config + auto-connect WebSocket if `config.websocket` is true.
    /// Convenience async constructor — equivalent to `new()` + `connect_ws()`.
    pub async fn new_async(config: &LlmConfig) -> Self {
        let llm = Self::new(config);
        if config.websocket
            && let Err(e) = llm.connect_ws().await
        {
            tracing::warn!("WebSocket upgrade skipped: {}", e);
        }
        llm
    }

    /// Upgrade to WebSocket mode for lower latency (oxide backend only).
    /// No-op for genai/chat/cli. Prefer `new_async()` which auto-connects based on config.
    pub async fn connect_ws(&self) -> Result<(), SgrError> {
        #[cfg(feature = "oxide-ws")]
        if let Backend::Oxide(c) = &self.inner {
            return c.inner().connect_ws().await;
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
        if let Some(err) = self.failure() {
            return Err(err);
        }
        match &self.inner {
            #[cfg(feature = "genai")]
            Backend::Genai(c) => c.stream_complete(messages, on_token).await,
            Backend::Oxide(_) | Backend::OxideChat(_) | Backend::Cli(_) => {
                // Non-streaming backends — generate full text,
                // then invoke on_token so callers (e.g. TTS, TUI) get the content.
                let text = self.generate(messages).await?;
                on_token(&text);
                Ok(text)
            }
            Backend::Failed(msg) => Err(SgrError::Schema(msg.clone())),
        }
    }

    /// Non-streaming text completion.
    pub async fn generate(&self, messages: &[Message]) -> Result<String, SgrError> {
        match self.client() {
            Some(c) => c.complete(messages).await,
            None => Err(self.failure().expect("client() None implies failure()")),
        }
    }

    /// Function calling with stateful session support (Responses API).
    /// Delegates to the trait method — each backend implements its own version.
    pub async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        match self.client() {
            Some(c) => {
                c.tools_call_stateful(messages, tools, previous_response_id)
                    .await
            }
            None => Err(self.failure().expect("client() None implies failure()")),
        }
    }

    /// Function calling that returns both tool calls and assistant text.
    /// Used by single-phase agents that need reasoning + action in one LLM call.
    pub async fn tools_call_with_text(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<(Vec<ToolCall>, String), SgrError> {
        match self.client() {
            Some(c) => c.tools_call_with_text(messages, tools).await,
            None => Err(self.failure().expect("client() None implies failure()")),
        }
    }

    /// Structured output — generates JSON schema from `T`, parses result.
    pub async fn structured<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
    ) -> Result<T, SgrError> {
        let client = self
            .client()
            .ok_or_else(|| self.failure().expect("client() None implies failure()"))?;
        let schema = response_schema_for::<T>();
        let (parsed, _tool_calls, raw_text) = client.structured_call(messages, &schema).await?;
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
            Backend::Cli(_) => "cli",
            Backend::Failed(_) => "failed",
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
        match self.client() {
            Some(c) => c.structured_call(messages, schema).await,
            None => Err(self.failure().expect("client() None implies failure()")),
        }
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        match self.client() {
            Some(c) => c.tools_call(messages, tools).await,
            None => Err(self.failure().expect("client() None implies failure()")),
        }
    }

    async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        match self.client() {
            Some(c) => {
                c.tools_call_stateful(messages, tools, previous_response_id)
                    .await
            }
            None => Err(self.failure().expect("client() None implies failure()")),
        }
    }

    async fn tools_call_with_text(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<(Vec<ToolCall>, String), SgrError> {
        match self.client() {
            Some(c) => c.tools_call_with_text(messages, tools).await,
            None => Err(self.failure().expect("client() None implies failure()")),
        }
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        match self.client() {
            Some(c) => c.complete(messages).await,
            None => Err(self.failure().expect("client() None implies failure()")),
        }
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

    /// Issue #2: previously panicked when there was no api_key + no base_url + no genai feature.
    /// Must now return a Failed backend that surfaces a clear error on first call.
    #[test]
    fn llm_without_key_or_url_does_not_panic() {
        // Snapshot + clear OPENAI_API_KEY so the test is hermetic.
        let saved = std::env::var("OPENAI_API_KEY").ok();
        // SAFETY: tests in the same process can race on env vars, but cargo serializes
        // `cargo test` within a single binary by default; this test is isolated enough.
        unsafe {
            std::env::remove_var("OPENAI_API_KEY");
        }

        let config = LlmConfig {
            model: "ollama-local-model".into(),
            api_key: None,
            base_url: None,
            ..Default::default()
        };
        let llm = Llm::new(&config);

        // Restore env var before any assertion (so a failure doesn't leak state).
        if let Some(v) = saved {
            unsafe {
                std::env::set_var("OPENAI_API_KEY", v);
            }
        }

        // Without the genai feature we expect a Failed backend; with it we expect genai fallback.
        #[cfg(not(feature = "genai"))]
        assert_eq!(llm.backend_name(), "failed");
        #[cfg(feature = "genai")]
        assert_eq!(llm.backend_name(), "genai");
    }
}
