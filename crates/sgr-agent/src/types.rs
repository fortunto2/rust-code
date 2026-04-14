use serde::{Deserialize, Serialize};

/// Inline image data for multimodal messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImagePart {
    /// Base64-encoded image data.
    pub data: String,
    /// MIME type (e.g. "image/jpeg", "image/png").
    pub mime_type: String,
}

/// A chat message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool call results (only for Role::Tool).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Tool calls made by the assistant (only for Role::Assistant with function calling).
    /// Gemini API requires model turns to include functionCall parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// Inline images (for multimodal VLM input).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImagePart>,
    /// Whether this message can be dropped during context compaction.
    /// false (default) = critical — never remove (inbox, instruction, system).
    /// true = compactable — can be summarized or dropped when context overflows.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compactable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            images: vec![],
            compactable: false,
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            images: vec![],
            compactable: false,
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            images: vec![],
            compactable: false,
        }
    }
    /// Create an assistant message that includes function calls (for Gemini FC protocol).
    pub fn assistant_with_tool_calls(
        content: impl Into<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            tool_call_id: None,
            tool_calls,
            images: vec![],
            compactable: false,
        }
    }
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(call_id.into()),
            tool_calls: vec![],
            images: vec![],
            compactable: false,
        }
    }
    /// Tool result with inline images (for VLM — Gemini sees the images).
    pub fn tool_with_images(
        call_id: impl Into<String>,
        content: impl Into<String>,
        images: Vec<ImagePart>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            tool_call_id: Some(call_id.into()),
            tool_calls: vec![],
            images,
            compactable: false,
        }
    }
    /// User message with inline images.
    pub fn user_with_images(content: impl Into<String>, images: Vec<ImagePart>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            tool_call_id: None,
            tool_calls: vec![],
            images,
            compactable: false,
        }
    }
    /// Mark this message as compactable (safe to drop during context overflow).
    pub fn compactable(mut self) -> Self {
        self.compactable = true;
        self
    }
}

/// A tool call returned by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique ID for matching with tool results.
    pub id: String,
    /// Tool/function name.
    pub name: String,
    /// JSON-encoded arguments.
    pub arguments: serde_json::Value,
}

/// Response from an SGR call — structured output + optional tool calls.
#[derive(Debug, Clone)]
pub struct SgrResponse<T> {
    /// Parsed structured output (SGR envelope).
    /// `None` if the model only returned tool calls without structured content.
    pub output: Option<T>,
    /// Tool calls the model wants to execute.
    pub tool_calls: Vec<ToolCall>,
    /// Raw text (for streaming / debugging).
    pub raw_text: String,
    /// Token usage.
    pub usage: Option<Usage>,
    /// Rate limit info from response headers (if provider sends them).
    pub rate_limit: Option<RateLimitInfo>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Rate limit info extracted from response headers and/or error body.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RateLimitInfo {
    /// Requests remaining in current window.
    pub requests_remaining: Option<u32>,
    /// Tokens remaining in current window.
    pub tokens_remaining: Option<u32>,
    /// Seconds until limit resets.
    pub retry_after_secs: Option<u64>,
    /// Unix timestamp when limit resets.
    pub resets_at: Option<u64>,
    /// Provider error type (e.g. "usage_limit_reached", "rate_limit_exceeded").
    pub error_type: Option<String>,
    /// Human-readable message from provider.
    pub message: Option<String>,
}

impl RateLimitInfo {
    /// Parse from HTTP response headers (OpenAI/Gemini/OpenRouter standard).
    pub fn from_headers(headers: &reqwest::header::HeaderMap) -> Option<Self> {
        let get_u32 =
            |name: &str| -> Option<u32> { headers.get(name)?.to_str().ok()?.parse().ok() };
        let get_u64 =
            |name: &str| -> Option<u64> { headers.get(name)?.to_str().ok()?.parse().ok() };

        let requests_remaining = get_u32("x-ratelimit-remaining-requests");
        let tokens_remaining = get_u32("x-ratelimit-remaining-tokens");
        let retry_after_secs =
            get_u64("retry-after").or_else(|| get_u64("x-ratelimit-reset-requests"));
        let resets_at = get_u64("x-ratelimit-reset-tokens");

        if requests_remaining.is_some() || tokens_remaining.is_some() || retry_after_secs.is_some()
        {
            Some(Self {
                requests_remaining,
                tokens_remaining,
                retry_after_secs,
                resets_at,
                error_type: None,
                message: None,
            })
        } else {
            None
        }
    }

    /// Parse from JSON error body (OpenAI, Codex, Gemini error responses).
    pub fn from_error_body(body: &str) -> Option<Self> {
        let json: serde_json::Value = serde_json::from_str(body).ok()?;
        let err = json.get("error")?;

        let error_type = err.get("type").and_then(|v| v.as_str()).map(String::from);
        let message = err
            .get("message")
            .and_then(|v| v.as_str())
            .map(String::from);
        let resets_at = err.get("resets_at").and_then(|v| v.as_u64());
        let retry_after_secs = err.get("resets_in_seconds").and_then(|v| v.as_u64());

        Some(Self {
            requests_remaining: None,
            tokens_remaining: None,
            retry_after_secs,
            resets_at,
            error_type,
            message,
        })
    }

    /// Human-readable description of when limit resets.
    pub fn reset_display(&self) -> String {
        if let Some(secs) = self.retry_after_secs {
            let hours = secs / 3600;
            let mins = (secs % 3600) / 60;
            if hours >= 24 {
                format!("{}d {}h", hours / 24, hours % 24)
            } else if hours > 0 {
                format!("{}h {}m", hours, mins)
            } else {
                format!("{}m", mins)
            }
        } else {
            "unknown".into()
        }
    }

    /// One-line status for status bar.
    pub fn status_line(&self) -> String {
        let mut parts = Vec::new();
        if let Some(r) = self.requests_remaining {
            parts.push(format!("req:{}", r));
        }
        if let Some(t) = self.tokens_remaining {
            parts.push(format!("tok:{}", t));
        }
        if self.retry_after_secs.is_some() {
            parts.push(format!("reset:{}", self.reset_display()));
        }
        if parts.is_empty() {
            self.message
                .clone()
                .unwrap_or_else(|| "rate limited".into())
        } else {
            parts.join(" | ")
        }
    }
}

/// LLM provider configuration — single config for any provider.
///
/// Two optional fields control routing:
/// - `api_key`: None → auto from env vars (OPENAI_API_KEY, ANTHROPIC_API_KEY, etc.)
/// - `base_url`: None → auto-detect provider from model name; Some → custom endpoint
///
/// ```no_run
/// use sgr_agent::LlmConfig;
///
/// let c = LlmConfig::auto("gpt-4o");                                          // env vars
/// let c = LlmConfig::with_key("sk-...", "claude-3-haiku");                    // explicit key
/// let c = LlmConfig::endpoint("sk-or-...", "https://openrouter.ai/api/v1", "gpt-4o"); // custom
/// let c = LlmConfig::auto("gpt-4o").temperature(0.9).max_tokens(2048);        // builder
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default = "default_temperature")]
    pub temp: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// OpenAI prompt cache key — caches system prompt prefix server-side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_key: Option<String>,
    /// Vertex AI project ID (enables Vertex routing when set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Vertex AI location (default: "global").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Force Chat Completions API instead of Responses API.
    /// Needed for OpenAI-compatible endpoints that don't support /responses
    /// (e.g. Cloudflare AI Gateway compat, OpenRouter, local models).
    #[serde(default)]
    pub use_chat_api: bool,
    /// Extra HTTP headers to include in LLM API requests.
    /// E.g. `cf-aig-request-timeout: 300000` for Cloudflare AI Gateway.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_headers: Vec<(String, String)>,
    /// Reasoning effort for reasoning models. "none" disables reasoning for FC.
    /// E.g. DeepInfra Nemotron Super needs "none" for function calling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Force genai backend (for providers with native API: Anthropic, Gemini).
    /// When false, oxide (OpenAI Responses API) is used by default.
    #[serde(default)]
    pub use_genai: bool,
    /// Use CLI subprocess backend (claude/gemini/codex -p).
    /// Tool calls emulated via text prompt + flexible parsing.
    /// Uses CLI's own auth (subscription credits, no API key).
    #[serde(default)]
    pub use_cli: bool,
    /// Session ID for request grouping (sticky routing, trace correlation).
    /// Set per-trial to group all LLM calls in the same session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

fn default_temperature() -> f64 {
    0.7
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            api_key: None,
            base_url: None,
            temp: default_temperature(),
            max_tokens: None,
            prompt_cache_key: None,
            project_id: None,
            location: None,
            use_chat_api: false,
            extra_headers: Vec::new(),
            reasoning_effort: None,
            use_genai: false,
            use_cli: false,
            session_id: None,
        }
    }
}

impl LlmConfig {
    /// Auto-detect provider from model name, use env vars for auth.
    pub fn auto(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            ..Default::default()
        }
    }

    /// Explicit API key, auto-detect provider from model name.
    pub fn with_key(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            api_key: Some(api_key.into()),
            ..Default::default()
        }
    }

    /// Custom OpenAI-compatible endpoint (OpenRouter, Ollama, LiteLLM, etc.).
    pub fn endpoint(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            model: model.into(),
            api_key: Some(api_key.into()),
            base_url: Some(base_url.into()),
            ..Default::default()
        }
    }

    /// Vertex AI — uses gcloud ADC for auth (no API key needed).
    pub fn vertex(project_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            project_id: Some(project_id.into()),
            location: Some("global".into()),
            ..Default::default()
        }
    }

    /// Set Vertex AI location.
    pub fn location(mut self, loc: impl Into<String>) -> Self {
        self.location = Some(loc.into());
        self
    }

    /// Set temperature.
    pub fn temperature(mut self, t: f64) -> Self {
        self.temp = t;
        self
    }

    /// Set max output tokens.
    pub fn max_tokens(mut self, m: u32) -> Self {
        self.max_tokens = Some(m);
        self
    }

    /// Set OpenAI prompt cache key for server-side system prompt caching.
    pub fn prompt_cache_key(mut self, key: impl Into<String>) -> Self {
        self.prompt_cache_key = Some(key.into());
        self
    }

    /// True if model targets Anthropic (via OpenRouter prefix or direct Claude model).
    pub fn is_anthropic(&self) -> bool {
        self.model.starts_with("anthropic/") || self.model.starts_with("claude")
    }

    /// Apply extra_headers to an openai-oxide ClientConfig.
    /// Used by both OxideClient and OxideChatClient.
    pub fn apply_headers(&self, config: &mut openai_oxide::config::ClientConfig) {
        if !self.extra_headers.is_empty() {
            let mut hm = reqwest::header::HeaderMap::new();
            for (k, v) in &self.extra_headers {
                if let (Ok(name), Ok(val)) = (
                    reqwest::header::HeaderName::from_bytes(k.as_bytes()),
                    reqwest::header::HeaderValue::from_str(v),
                ) {
                    hm.insert(name, val);
                }
            }
            config.default_headers = Some(hm);
        }
    }

    /// CLI subprocess backend — uses `claude -p` / `gemini -p` / `codex exec`.
    /// No API key needed, uses CLI's own auth (subscription credits).
    /// Optional `model` overrides the CLI's default model via `--model` flag.
    pub fn cli(cli_model: impl Into<String>) -> Self {
        Self {
            model: cli_model.into(),
            use_cli: true,
            ..Default::default()
        }
    }

    /// Human-readable label for display.
    pub fn label(&self) -> String {
        if self.use_cli {
            format!("CLI ({})", self.model)
        } else if self.project_id.is_some() {
            format!("Vertex ({})", self.model)
        } else if self.base_url.is_some() {
            format!("Custom ({})", self.model)
        } else {
            self.model.clone()
        }
    }

    /// Infer a cheap/fast model for compaction based on the primary model.
    pub fn compaction_model(&self) -> String {
        if self.model.starts_with("gemini") {
            "gemini-2.0-flash-lite".into()
        } else if self.model.starts_with("gpt") {
            "gpt-4o-mini".into()
        } else if self.model.starts_with("claude") {
            "claude-3-haiku-20240307".into()
        } else {
            // Unknown provider — use the same model
            self.model.clone()
        }
    }

    /// Create a compaction config — cheap model, low max_tokens.
    pub fn for_compaction(&self) -> Self {
        let mut cfg = self.clone();
        cfg.model = self.compaction_model();
        cfg.max_tokens = Some(2048);
        cfg
    }
}

/// Legacy provider configuration (used by OpenAIClient/GeminiClient).
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    pub project_id: Option<String>,
    pub location: Option<String>,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
}

impl ProviderConfig {
    pub fn gemini(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: None,
            project_id: None,
            location: None,
            temperature: 0.3,
            max_tokens: None,
        }
    }

    pub fn openai(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: None,
            project_id: None,
            location: None,
            temperature: 0.3,
            max_tokens: None,
        }
    }

    pub fn openrouter(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            model: model.into(),
            base_url: Some("https://openrouter.ai/api/v1".into()),
            project_id: None,
            location: None,
            temperature: 0.3,
            max_tokens: None,
        }
    }

    pub fn vertex(
        access_token: impl Into<String>,
        project_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            api_key: access_token.into(),
            model: model.into(),
            base_url: None,
            project_id: Some(project_id.into()),
            location: Some("global".to_string()),
            temperature: 0.3,
            max_tokens: None,
        }
    }

    pub fn ollama(model: impl Into<String>) -> Self {
        Self {
            api_key: String::new(),
            model: model.into(),
            base_url: Some("http://localhost:11434/v1".into()),
            project_id: None,
            location: None,
            temperature: 0.3,
            max_tokens: None,
        }
    }
}

/// Errors from SGR calls.
#[derive(Debug, thiserror::Error)]
pub enum SgrError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("API error {status}: {body}")]
    Api { status: u16, body: String },
    #[error("Rate limit: {}", info.status_line())]
    RateLimit { status: u16, info: RateLimitInfo },
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Schema error: {0}")]
    Schema(String),
    #[error("No content in response")]
    EmptyResponse,
    /// Model response was truncated due to max_output_tokens limit.
    /// Contains the partial content that was generated before truncation.
    #[error("Response truncated (max_output_tokens): {partial_content}")]
    MaxOutputTokens { partial_content: String },
    /// Prompt too long — context exceeds model's input limit.
    #[error("Prompt too long: {0}")]
    PromptTooLong(String),
}

impl SgrError {
    /// Build error from HTTP status + body, auto-detecting rate limits.
    pub fn from_api_response(status: u16, body: String) -> Self {
        if (status == 429 || body.contains("usage_limit_reached") || body.contains("rate_limit"))
            && let Some(mut info) = RateLimitInfo::from_error_body(&body)
        {
            if info.message.is_none() {
                info.message = Some(body.chars().take(200).collect());
            }
            return SgrError::RateLimit { status, info };
        }
        SgrError::Api { status, body }
    }

    /// Build error from HTTP status + body + headers, auto-detecting rate limits.
    pub fn from_response_parts(
        status: u16,
        body: String,
        headers: &reqwest::header::HeaderMap,
    ) -> Self {
        if status == 429 || body.contains("usage_limit_reached") || body.contains("rate_limit") {
            let mut info = RateLimitInfo::from_error_body(&body)
                .or_else(|| RateLimitInfo::from_headers(headers))
                .unwrap_or(RateLimitInfo {
                    requests_remaining: None,
                    tokens_remaining: None,
                    retry_after_secs: None,
                    resets_at: None,
                    error_type: Some("rate_limit".into()),
                    message: Some(body.chars().take(200).collect()),
                });
            // Merge header info into body info
            if let Some(header_info) = RateLimitInfo::from_headers(headers) {
                if info.requests_remaining.is_none() {
                    info.requests_remaining = header_info.requests_remaining;
                }
                if info.tokens_remaining.is_none() {
                    info.tokens_remaining = header_info.tokens_remaining;
                }
            }
            return SgrError::RateLimit { status, info };
        }
        SgrError::Api { status, body }
    }

    /// Is this a rate limit error?
    pub fn is_rate_limit(&self) -> bool {
        matches!(self, SgrError::RateLimit { .. })
    }

    /// Get rate limit info if this is a rate limit error.
    pub fn rate_limit_info(&self) -> Option<&RateLimitInfo> {
        match self {
            SgrError::RateLimit { info, .. } => Some(info),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_codex_rate_limit_error() {
        let body = r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"plus","resets_at":1773534007,"resets_in_seconds":442787}}"#;
        let err = SgrError::from_api_response(429, body.to_string());
        assert!(err.is_rate_limit());
        let info = err.rate_limit_info().unwrap();
        assert_eq!(info.error_type.as_deref(), Some("usage_limit_reached"));
        assert_eq!(info.retry_after_secs, Some(442787));
        assert_eq!(info.resets_at, Some(1773534007));
        assert_eq!(info.reset_display(), "5d 2h");
    }

    #[test]
    fn parse_openai_rate_limit_error() {
        let body =
            r#"{"error":{"type":"rate_limit_exceeded","message":"Rate limit reached for gpt-4"}}"#;
        let err = SgrError::from_api_response(429, body.to_string());
        assert!(err.is_rate_limit());
        let info = err.rate_limit_info().unwrap();
        assert_eq!(info.error_type.as_deref(), Some("rate_limit_exceeded"));
    }

    #[test]
    fn non_rate_limit_stays_api_error() {
        let body = r#"{"error":{"type":"invalid_request","message":"Bad request"}}"#;
        let err = SgrError::from_api_response(400, body.to_string());
        assert!(!err.is_rate_limit());
        assert!(matches!(err, SgrError::Api { status: 400, .. }));
    }

    #[test]
    fn status_line_with_all_fields() {
        let info = RateLimitInfo {
            requests_remaining: Some(5),
            tokens_remaining: Some(10000),
            retry_after_secs: Some(3600),
            resets_at: None,
            error_type: None,
            message: None,
        };
        assert_eq!(info.status_line(), "req:5 | tok:10000 | reset:1h 0m");
    }

    #[test]
    fn status_line_fallback_to_message() {
        let info = RateLimitInfo {
            requests_remaining: None,
            tokens_remaining: None,
            retry_after_secs: None,
            resets_at: None,
            error_type: None,
            message: Some("custom message".into()),
        };
        assert_eq!(info.status_line(), "custom message");
    }

    #[test]
    fn reset_display_formats() {
        let make = |secs| RateLimitInfo {
            requests_remaining: None,
            tokens_remaining: None,
            retry_after_secs: Some(secs),
            resets_at: None,
            error_type: None,
            message: None,
        };
        assert_eq!(make(90).reset_display(), "1m");
        assert_eq!(make(3661).reset_display(), "1h 1m");
        assert_eq!(make(90000).reset_display(), "1d 1h");
    }
}
