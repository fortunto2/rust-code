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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
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
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Schema error: {0}")]
    Schema(String),
    #[error("No content in response")]
    EmptyResponse,
}
