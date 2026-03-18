//! OpenAI-compatible API client — works with OpenAI, OpenRouter, Ollama.
//!
//! Combines:
//! - **Structured output**: `response_format.type = "json_schema"` — typed responses
//! - **Function calling**: `tools[]` — model returns `tool_calls` in the response
//!
//! Both can be used together in a single request.

use crate::schema::response_schema_for;
use crate::tool::ToolDef;
use crate::types::*;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

/// OpenAI-compatible API client.
pub struct OpenAIClient {
    config: ProviderConfig,
    http: reqwest::Client,
}

impl OpenAIClient {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Quick constructor for OpenRouter.
    pub fn openrouter(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(ProviderConfig::openrouter(api_key, model))
    }

    /// Quick constructor for Ollama (local).
    pub fn ollama(model: impl Into<String>) -> Self {
        Self::new(ProviderConfig::ollama(model))
    }

    /// SGR call: structured output + function calling.
    pub async fn call<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<SgrResponse<T>, SgrError> {
        let body = self.build_request::<T>(messages, tools);
        let url = self.build_url();

        tracing::debug!(url = %url, model = %self.config.model, "openai_request");

        let mut request = self.http.post(&url).json(&body);

        if !self.config.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let response = request.send().await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(SgrError::from_response_parts(status, body, &headers));
        }

        let response_body: Value = response.json().await?;
        let rate_limit = RateLimitInfo::from_headers(&headers);
        self.parse_response(&response_body, rate_limit)
    }

    /// Structured output only (no tools).
    pub async fn structured<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
    ) -> Result<T, SgrError> {
        let resp = self.call::<T>(messages, &[]).await?;
        resp.output.ok_or(SgrError::EmptyResponse)
    }

    /// Flexible call: no structured output API, parse JSON from raw text.
    ///
    /// For use with text-only proxies (CLI proxy, Codex proxy, Ollama without grammar).
    /// Uses AnyOf cascade + coercion.
    ///
    /// Auto-injects JSON Schema into the system prompt so the model knows
    /// the expected format (like BAML does).
    pub async fn flexible<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
    ) -> Result<SgrResponse<T>, SgrError> {
        // Auto-inject schema hint into messages
        let schema = crate::schema::response_schema_for::<T>();
        let schema_hint = format!(
            "\n\nRespond with valid JSON matching this schema:\n{}\n\nDo NOT wrap in markdown code blocks.",
            serde_json::to_string_pretty(&schema).unwrap_or_default()
        );
        let mut augmented_msgs = messages.to_vec();
        // Append schema hint to existing system message or add one
        let has_system = augmented_msgs.iter().any(|m| m.role == Role::System);
        if has_system {
            for msg in &mut augmented_msgs {
                if msg.role == Role::System {
                    msg.content.push_str(&schema_hint);
                    break;
                }
            }
        } else {
            augmented_msgs.insert(0, Message::system(schema_hint));
        }

        // Send without response_format — plain text
        let msgs = self.messages_to_openai(&augmented_msgs);
        let mut body = json!({
            "model": self.config.model,
            "messages": msgs,
            "temperature": self.config.temperature,
        });
        if let Some(max_tokens) = self.config.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        let url = self.build_url();
        let mut request = self.http.post(&url).json(&body);
        if !self.config.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let response = request.send().await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(SgrError::from_response_parts(status, body, &headers));
        }

        let response_body: Value = response.json().await?;
        let rate_limit = RateLimitInfo::from_headers(&headers);

        // Extract raw text and usage
        let raw_text = self.extract_raw_text(&response_body);
        let usage = response_body.get("usage").and_then(|u| {
            Some(Usage {
                prompt_tokens: u.get("prompt_tokens")?.as_u64()? as u32,
                completion_tokens: u.get("completion_tokens")?.as_u64()? as u32,
                total_tokens: u.get("total_tokens")?.as_u64()? as u32,
            })
        });

        // Flexible parse with coercion
        let output = crate::flexible_parser::parse_flexible_coerced::<T>(&raw_text)
            .map(|r| r.value)
            .ok();

        if output.is_none() && raw_text.trim().is_empty() {
            return Err(SgrError::Schema("Empty response from model".into()));
        }

        Ok(SgrResponse {
            output,
            tool_calls: vec![],
            raw_text,
            usage,
            rate_limit,
        })
    }

    /// Tool-only call.
    pub async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        let body = self.build_tools_only_request(messages, tools);
        let url = self.build_url();

        let mut request = self.http.post(&url).json(&body);
        if !self.config.api_key.is_empty() {
            request = request.header("Authorization", format!("Bearer {}", self.config.api_key));
        }

        let response = request.send().await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(SgrError::from_response_parts(status, body, &headers));
        }

        let response_body: Value = response.json().await?;
        Ok(self.extract_tool_calls(&response_body))
    }

    // --- Private ---

    fn build_url(&self) -> String {
        let base = self
            .config
            .base_url
            .as_deref()
            .unwrap_or("https://api.openai.com/v1");
        format!("{}/chat/completions", base)
    }

    fn build_request<T: JsonSchema>(&self, messages: &[Message], tools: &[ToolDef]) -> Value {
        let msgs = self.messages_to_openai(messages);
        let mut schema = response_schema_for::<T>();
        // OpenAI strict mode: additionalProperties:false + all properties required
        crate::schema::make_openai_strict(&mut schema);

        let mut body = json!({
            "model": self.config.model,
            "messages": msgs,
            "temperature": self.config.temperature,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "sgr_response",
                    "strict": true,
                    "schema": schema,
                }
            }
        });

        if let Some(max_tokens) = self.config.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        if !tools.is_empty() {
            let tool_defs: Vec<Value> = tools.iter().map(|t| t.to_openai()).collect();
            body["tools"] = json!(tool_defs);
        }

        body
    }

    fn build_tools_only_request(&self, messages: &[Message], tools: &[ToolDef]) -> Value {
        let msgs = self.messages_to_openai(messages);
        let tool_defs: Vec<Value> = tools.iter().map(|t| t.to_openai()).collect();

        let mut body = json!({
            "model": self.config.model,
            "messages": msgs,
            "temperature": self.config.temperature,
            "tools": tool_defs,
            "tool_choice": "required",
        });

        if let Some(max_tokens) = self.config.max_tokens {
            body["max_tokens"] = json!(max_tokens);
        }

        body
    }

    fn messages_to_openai(&self, messages: &[Message]) -> Vec<Value> {
        messages
            .iter()
            .map(|msg| {
                let role = match msg.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                // Multimodal: if message has images, use content array format
                let content = if !msg.images.is_empty()
                    && (msg.role == Role::User || msg.role == Role::System)
                {
                    let mut parts: Vec<Value> = vec![json!({
                        "type": "text",
                        "text": msg.content,
                    })];
                    for img in &msg.images {
                        parts.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{};base64,{}", img.mime_type, img.data),
                            }
                        }));
                    }
                    json!(parts)
                } else {
                    json!(msg.content)
                };
                let mut m = json!({
                    "role": role,
                    "content": content,
                });
                if let Some(id) = &msg.tool_call_id {
                    m["tool_call_id"] = json!(id);
                }
                m
            })
            .collect()
    }

    fn parse_response<T: DeserializeOwned>(
        &self,
        body: &Value,
        rate_limit: Option<RateLimitInfo>,
    ) -> Result<SgrResponse<T>, SgrError> {
        let mut output: Option<T> = None;
        let mut tool_calls = Vec::new();
        let mut raw_text = String::new();

        let usage = body.get("usage").and_then(|u| {
            Some(Usage {
                prompt_tokens: u.get("prompt_tokens")?.as_u64()? as u32,
                completion_tokens: u.get("completion_tokens")?.as_u64()? as u32,
                total_tokens: u.get("total_tokens")?.as_u64()? as u32,
            })
        });

        let choices = body
            .get("choices")
            .and_then(|c| c.as_array())
            .ok_or(SgrError::EmptyResponse)?;

        for choice in choices {
            let message = choice.get("message").ok_or(SgrError::EmptyResponse)?;

            // Text content → structured output
            if let Some(content) = message.get("content").and_then(|c| c.as_str()) {
                raw_text.push_str(content);
                if output.is_none() && !content.is_empty() {
                    match serde_json::from_str::<T>(content) {
                        Ok(parsed) => output = Some(parsed),
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to parse structured output");
                        }
                    }
                }
            }

            // Tool calls
            if let Some(tcs) = message.get("tool_calls").and_then(|t| t.as_array()) {
                for tc in tcs {
                    let id = tc
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    if let Some(func) = tc.get("function") {
                        let name = func
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let args_str = func
                            .get("arguments")
                            .and_then(|a| a.as_str())
                            .unwrap_or("{}");
                        let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                        tool_calls.push(ToolCall {
                            id,
                            name,
                            arguments: args,
                        });
                    }
                }
            }
        }

        if output.is_none() && tool_calls.is_empty() {
            return Err(SgrError::EmptyResponse);
        }

        Ok(SgrResponse {
            output,
            tool_calls,
            raw_text,
            usage,
            rate_limit,
        })
    }

    fn extract_raw_text(&self, body: &Value) -> String {
        let mut text = String::new();
        if let Some(choices) = body.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(content) = choice
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    text.push_str(content);
                }
            }
        }
        text
    }

    fn extract_tool_calls(&self, body: &Value) -> Vec<ToolCall> {
        let mut calls = Vec::new();
        if let Some(choices) = body.get("choices").and_then(|c| c.as_array()) {
            for choice in choices {
                if let Some(tcs) = choice
                    .get("message")
                    .and_then(|m| m.get("tool_calls"))
                    .and_then(|t| t.as_array())
                {
                    for tc in tcs {
                        let id = tc
                            .get("id")
                            .and_then(|i| i.as_str())
                            .unwrap_or("")
                            .to_string();
                        if let Some(func) = tc.get("function") {
                            let name = func
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("")
                                .to_string();
                            let args_str = func
                                .get("arguments")
                                .and_then(|a| a.as_str())
                                .unwrap_or("{}");
                            let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                            calls.push(ToolCall {
                                id,
                                name,
                                arguments: args,
                            });
                        }
                    }
                }
            }
        }
        calls
    }
}
