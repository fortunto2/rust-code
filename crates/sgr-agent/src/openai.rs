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
use serde_json::{json, Value};

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
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(SgrError::Api { status, body });
        }

        let response_body: Value = response.json().await?;
        self.parse_response(&response_body)
    }

    /// Structured output only (no tools).
    pub async fn structured<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
    ) -> Result<T, SgrError> {
        let resp = self.call::<T>(messages, &[]).await?;
        resp.output.ok_or(SgrError::EmptyResponse)
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
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(SgrError::Api { status, body });
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
        let schema = response_schema_for::<T>();

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
                let mut m = json!({
                    "role": role,
                    "content": msg.content,
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
                        let args: Value =
                            serde_json::from_str(args_str).unwrap_or(json!({}));
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
        })
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
                        let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                        if let Some(func) = tc.get("function") {
                            let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                            let args_str = func.get("arguments").and_then(|a| a.as_str()).unwrap_or("{}");
                            let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));
                            calls.push(ToolCall { id, name, arguments: args });
                        }
                    }
                }
            }
        }
        calls
    }
}
