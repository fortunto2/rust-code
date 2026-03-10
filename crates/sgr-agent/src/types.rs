use serde::{Deserialize, Serialize};

/// A chat message in the conversation history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
    /// Tool call results (only for Role::Tool).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
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
        Self { role: Role::System, content: content.into(), tool_call_id: None }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: Role::User, content: content.into(), tool_call_id: None }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: Role::Assistant, content: content.into(), tool_call_id: None }
    }
    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self { role: Role::Tool, content: content.into(), tool_call_id: Some(call_id.into()) }
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
        let get_u32 = |name: &str| -> Option<u32> {
            headers.get(name)?.to_str().ok()?.parse().ok()
        };
        let get_u64 = |name: &str| -> Option<u64> {
            headers.get(name)?.to_str().ok()?.parse().ok()
        };

        let requests_remaining = get_u32("x-ratelimit-remaining-requests");
        let tokens_remaining = get_u32("x-ratelimit-remaining-tokens");
        let retry_after_secs = get_u64("retry-after")
            .or_else(|| get_u64("x-ratelimit-reset-requests"));
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
        let message = err.get("message").and_then(|v| v.as_str()).map(String::from);
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
            self.message.clone().unwrap_or_else(|| "rate limited".into())
        } else {
            parts.join(" | ")
        }
    }
}

/// Provider configuration.
#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    /// Vertex AI project ID (for Vertex auth).
    pub project_id: Option<String>,
    /// Vertex AI location.
    pub location: Option<String>,
    /// Temperature (0.0 - 2.0).
    pub temperature: f32,
    /// Max output tokens.
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
            location: Some("us-central1".to_string()),
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
}

impl SgrError {
    /// Build error from HTTP status + body, auto-detecting rate limits.
    pub fn from_api_response(status: u16, body: String) -> Self {
        if status == 429 || body.contains("usage_limit_reached") || body.contains("rate_limit") {
            if let Some(mut info) = RateLimitInfo::from_error_body(&body) {
                if info.message.is_none() {
                    info.message = Some(body.chars().take(200).collect());
                }
                return SgrError::RateLimit { status, info };
            }
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
        let body = r#"{"error":{"type":"rate_limit_exceeded","message":"Rate limit reached for gpt-4"}}"#;
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
