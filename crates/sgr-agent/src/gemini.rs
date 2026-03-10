//! Gemini API client — structured output + function calling.
//!
//! Supports both Google AI Studio (API key) and Vertex AI (ADC).
//!
//! Two modes combined:
//! - **Structured output**: `generationConfig.responseMimeType = "application/json"`
//!   + `responseSchema` — forces model to return JSON matching the SGR envelope.
//! - **Function calling**: `tools[].functionDeclarations` — model emits `functionCall`
//!   parts that map to your Rust tool structs.
//!
//! The model can return BOTH structured text AND function calls in one response.

use crate::schema::response_schema_for;
use crate::tool::ToolDef;
use crate::types::*;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

/// Gemini API client.
pub struct GeminiClient {
    config: ProviderConfig,
    http: reqwest::Client,
}

impl GeminiClient {
    pub fn new(config: ProviderConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    /// Quick constructor for Google AI Studio (API key auth).
    pub fn from_api_key(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self::new(ProviderConfig::gemini(api_key, model))
    }

    /// SGR call: structured output (typed response) + function calling (tools).
    ///
    /// Returns `SgrResponse<T>` where:
    /// - `output`: parsed structured response (if model returned text)
    /// - `tool_calls`: function calls (if model used tools)
    ///
    /// The model may return either or both.
    pub async fn call<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<SgrResponse<T>, SgrError> {
        let body = self.build_request::<T>(messages, tools)?;
        let url = self.build_url();

        tracing::debug!(url = %url, model = %self.config.model, "gemini_request");

        let mut req = self.http.post(&url).json(&body);
        if self.config.project_id.is_some() && !self.config.api_key.is_empty() {
            req = req.bearer_auth(&self.config.api_key);
        }
        let response = req.send().await?;

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

    /// SGR call with structured output only (no tools).
    ///
    /// Shorthand for `call::<T>(messages, &[])`.
    pub async fn structured<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
    ) -> Result<T, SgrError> {
        let resp = self.call::<T>(messages, &[]).await?;
        resp.output.ok_or(SgrError::EmptyResponse)
    }

    /// Flexible call: no structured output API, parse JSON from raw text.
    ///
    /// For use with text-only proxies (CLI proxy, Codex proxy) where
    /// the model can't enforce JSON schema. Uses AnyOf cascade + coercion.
    ///
    /// Auto-injects JSON Schema into the system prompt so the model knows
    /// the expected format (like BAML does).
    pub async fn flexible<T: JsonSchema + DeserializeOwned>(
        &self,
        messages: &[Message],
    ) -> Result<SgrResponse<T>, SgrError> {
        // Send without responseSchema — plain text response
        // Use text mode for tool messages (no functionDeclarations in this mode)
        let contents = self.messages_to_contents_text(messages);
        let mut system_instruction = self.extract_system(messages);

        // Auto-inject schema hint into system prompt
        let schema = response_schema_for::<T>();
        let schema_hint = format!(
            "\n\nRespond with valid JSON matching this schema:\n{}\n\nDo NOT wrap in markdown code blocks.",
            serde_json::to_string_pretty(&schema).unwrap_or_default()
        );
        system_instruction = Some(match system_instruction {
            Some(s) => format!("{}{}", s, schema_hint),
            None => schema_hint,
        });

        let mut gen_config = json!({
            "temperature": self.config.temperature,
        });
        if let Some(max_tokens) = self.config.max_tokens {
            gen_config["maxOutputTokens"] = json!(max_tokens);
        }

        let mut body = json!({
            "contents": contents,
            "generationConfig": gen_config,
        });
        if let Some(system) = system_instruction {
            body["systemInstruction"] = json!({
                "parts": [{"text": system}]
            });
        }

        let url = self.build_url();
        let mut req = self.http.post(&url).json(&body);
        if self.config.project_id.is_some() && !self.config.api_key.is_empty() {
            req = req.bearer_auth(&self.config.api_key);
        }
        let response = req.send().await?;
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(SgrError::from_response_parts(status, body, &headers));
        }

        let response_body: Value = response.json().await?;
        let rate_limit = RateLimitInfo::from_headers(&headers);

        // Extract raw text
        let raw_text = self.extract_raw_text(&response_body);
        if raw_text.trim().is_empty() {
            // Log finish reason and response parts for debugging
            if let Some(candidate) = response_body
                .get("candidates")
                .and_then(|c| c.get(0))
            {
                let reason = candidate
                    .get("finishReason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("unknown");
                tracing::warn!(
                    finish_reason = reason,
                    has_parts = candidate.get("content").and_then(|c| c.get("parts")).is_some(),
                    "empty raw_text from Gemini"
                );
            }
        }
        let usage = response_body.get("usageMetadata").and_then(|u| {
            Some(Usage {
                prompt_tokens: u.get("promptTokenCount")?.as_u64()? as u32,
                completion_tokens: u.get("candidatesTokenCount")?.as_u64()? as u32,
                total_tokens: u.get("totalTokenCount")?.as_u64()? as u32,
            })
        });

        // Extract native function calls (Gemini may use functionCall parts
        // even without explicit functionDeclarations — especially newer models).
        let tool_calls = self.extract_tool_calls(&response_body);

        // Flexible parse with coercion.
        // If parsing fails, return output=None with raw_text preserved
        // so callers can implement fallback logic (e.g. wrap in finish tool).
        let output = crate::flexible_parser::parse_flexible_coerced::<T>(&raw_text)
            .map(|r| r.value)
            .ok();

        if output.is_none() && raw_text.trim().is_empty() && tool_calls.is_empty() {
            // Log raw response for debugging
            let parts_summary = response_body
                .get("candidates")
                .and_then(|c| c.get(0))
                .and_then(|c| c.get("content"))
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array())
                .map(|parts| {
                    parts.iter()
                        .map(|p| {
                            if p.get("text").is_some() { "text".to_string() }
                            else if p.get("functionCall").is_some() {
                                format!("functionCall:{}", p["functionCall"]["name"].as_str().unwrap_or("?"))
                            }
                            else { format!("unknown:{}", p) }
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_else(|| "no parts".into());
            // Log full candidate for debugging
            let candidate_json = response_body
                .get("candidates")
                .and_then(|c| c.get(0))
                .map(|c| serde_json::to_string_pretty(c).unwrap_or_default())
                .unwrap_or_else(|| "no candidates".into());
            tracing::error!(
                parts = parts_summary,
                candidate = candidate_json.as_str(),
                "SGR empty response"
            );
            return Err(SgrError::Schema(format!("Empty response from model (parts: {})", parts_summary)));
        }

        Ok(SgrResponse {
            output,
            tool_calls,
            raw_text,
            usage,
            rate_limit,
        })
    }

    /// Tool-only call: no structured output schema, just function calling.
    ///
    /// Returns raw tool calls.
    pub async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        let body = self.build_tools_only_request(messages, tools)?;
        let url = self.build_url();

        let mut req = self.http.post(&url).json(&body);
        if self.config.project_id.is_some() && !self.config.api_key.is_empty() {
            req = req.bearer_auth(&self.config.api_key);
        }
        let response = req.send().await?;
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
        if let Some(project_id) = &self.config.project_id {
            // Vertex AI
            let location = self.config.location.as_deref().unwrap_or("global");
            let host = if location == "global" {
                "aiplatform.googleapis.com".to_string()
            } else {
                format!("{location}-aiplatform.googleapis.com")
            };
            format!(
                "https://{host}/v1/projects/{project_id}/locations/{location}/publishers/google/models/{}:generateContent",
                self.config.model
            )
        } else {
            // Google AI Studio
            format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
                self.config.model, self.config.api_key
            )
        }
    }

    fn build_request<T: JsonSchema>(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Value, SgrError> {
        // Use functionResponse format only when tools are present
        let contents = if tools.is_empty() {
            self.messages_to_contents_text(messages)
        } else {
            self.messages_to_contents(messages)
        };
        let system_instruction = self.extract_system(messages);

        // When using function calling, Gemini doesn't support responseMimeType + functionDeclarations.
        // Use structured output (JSON mode) only when there are no tools.
        let mut gen_config = json!({
            "temperature": self.config.temperature,
        });

        if tools.is_empty() {
            gen_config["responseMimeType"] = json!("application/json");
            gen_config["responseSchema"] = response_schema_for::<T>();
        }

        if let Some(max_tokens) = self.config.max_tokens {
            gen_config["maxOutputTokens"] = json!(max_tokens);
        }

        let mut body = json!({
            "contents": contents,
            "generationConfig": gen_config,
        });

        if let Some(system) = system_instruction {
            body["systemInstruction"] = json!({
                "parts": [{"text": system}]
            });
        }

        if !tools.is_empty() {
            let function_declarations: Vec<Value> =
                tools.iter().map(|t| t.to_gemini()).collect();
            body["tools"] = json!([{
                "functionDeclarations": function_declarations,
            }]);
            body["toolConfig"] = json!({
                "functionCallingConfig": {
                    "mode": "AUTO"
                }
            });
        }

        Ok(body)
    }

    fn build_tools_only_request(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Value, SgrError> {
        let contents = self.messages_to_contents(messages);
        let system_instruction = self.extract_system(messages);

        let mut gen_config = json!({
            "temperature": self.config.temperature,
        });
        if let Some(max_tokens) = self.config.max_tokens {
            gen_config["maxOutputTokens"] = json!(max_tokens);
        }

        let function_declarations: Vec<Value> =
            tools.iter().map(|t| t.to_gemini()).collect();

        let mut body = json!({
            "contents": contents,
            "generationConfig": gen_config,
            "tools": [{
                "functionDeclarations": function_declarations,
            }],
            "toolConfig": {
                "functionCallingConfig": {
                    "mode": "ANY"
                }
            }
        });

        if let Some(system) = system_instruction {
            body["systemInstruction"] = json!({
                "parts": [{"text": system}]
            });
        }

        Ok(body)
    }

    /// Convert messages to Gemini contents format.
    ///
    /// When `use_function_response` is true, Tool messages become `functionResponse` parts
    /// (for native function calling mode). When false, they become user text messages
    /// (for structured output / flexible mode where no function declarations are sent).
    fn messages_to_contents(&self, messages: &[Message]) -> Vec<Value> {
        self.messages_to_contents_inner(messages, true)
    }

    fn messages_to_contents_text(&self, messages: &[Message]) -> Vec<Value> {
        self.messages_to_contents_inner(messages, false)
    }

    fn messages_to_contents_inner(&self, messages: &[Message], use_function_response: bool) -> Vec<Value> {
        let mut contents = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {} // handled separately via systemInstruction
                Role::User => {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{"text": msg.content}]
                    }));
                }
                Role::Assistant => {
                    contents.push(json!({
                        "role": "model",
                        "parts": [{"text": msg.content}]
                    }));
                }
                Role::Tool => {
                    if use_function_response {
                        // Native function calling mode — functionResponse parts
                        // call_id format: "call#name#counter" — extract the function name
                        let call_id = msg.tool_call_id.as_deref().unwrap_or("unknown");
                        let func_name = match call_id.split('#').collect::<Vec<_>>().as_slice() {
                            ["call", name, _counter] => *name,
                            _ => call_id, // fallback: old format or plain tool name
                        };
                        contents.push(json!({
                            "role": "function",
                            "parts": [{
                                "functionResponse": {
                                    "name": func_name,
                                    "response": {
                                        "content": msg.content,
                                    }
                                }
                            }]
                        }));
                    } else {
                        // Text mode — convert tool results to user messages
                        let call_id = msg.tool_call_id.as_deref().unwrap_or("tool");
                        contents.push(json!({
                            "role": "user",
                            "parts": [{"text": format!("[{}] {}", call_id, msg.content)}]
                        }));
                    }
                }
            }
        }

        contents
    }

    fn extract_system(&self, messages: &[Message]) -> Option<String> {
        let system_parts: Vec<&str> = messages
            .iter()
            .filter(|m| m.role == Role::System)
            .map(|m| m.content.as_str())
            .collect();

        if system_parts.is_empty() {
            None
        } else {
            Some(system_parts.join("\n\n"))
        }
    }

    fn parse_response<T: DeserializeOwned>(
        &self,
        body: &Value,
        rate_limit: Option<RateLimitInfo>,
    ) -> Result<SgrResponse<T>, SgrError> {
        let mut output: Option<T> = None;
        let mut tool_calls = Vec::new();
        let mut raw_text = String::new();
        let mut call_counter: u32 = 0;

        // Parse usage
        let usage = body.get("usageMetadata").and_then(|u| {
            Some(Usage {
                prompt_tokens: u.get("promptTokenCount")?.as_u64()? as u32,
                completion_tokens: u.get("candidatesTokenCount")?.as_u64()? as u32,
                total_tokens: u.get("totalTokenCount")?.as_u64()? as u32,
            })
        });

        // Extract from candidates
        let candidates = body
            .get("candidates")
            .and_then(|c| c.as_array())
            .ok_or(SgrError::EmptyResponse)?;

        for candidate in candidates {
            let parts = candidate
                .get("content")
                .and_then(|c| c.get("parts"))
                .and_then(|p| p.as_array());

            if let Some(parts) = parts {
                for part in parts {
                    // Text part → structured output
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        raw_text.push_str(text);
                        if output.is_none() {
                            match serde_json::from_str::<T>(text) {
                                Ok(parsed) => output = Some(parsed),
                                Err(e) => {
                                    tracing::warn!(error = %e, "failed to parse structured output");
                                }
                            }
                        }
                    }

                    // Function call part → tool call
                    if let Some(fc) = part.get("functionCall") {
                        let name = fc
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let args = fc.get("args").cloned().unwrap_or(json!({}));
                        call_counter += 1;
                        tool_calls.push(ToolCall {
                            id: format!("call#{}#{}", name, call_counter),
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
        if let Some(candidates) = body.get("candidates").and_then(|c| c.as_array()) {
            for candidate in candidates {
                if let Some(parts) = candidate
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    for part in parts {
                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                            text.push_str(t);
                        }
                    }
                }
            }
        }
        text
    }

    fn extract_tool_calls(&self, body: &Value) -> Vec<ToolCall> {
        let mut calls = Vec::new();

        if let Some(candidates) = body.get("candidates").and_then(|c| c.as_array()) {
            for candidate in candidates {
                // Standard: functionCall in parts
                if let Some(parts) = candidate
                    .get("content")
                    .and_then(|c| c.get("parts"))
                    .and_then(|p| p.as_array())
                {
                    let mut call_counter = 0u32;
                    for part in parts {
                        if let Some(fc) = part.get("functionCall") {
                            let name = fc
                                .get("name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let args = fc.get("args").cloned().unwrap_or(json!({}));
                            call_counter += 1;
                            calls.push(ToolCall {
                                id: format!("call#{}#{}", name, call_counter),
                                name,
                                arguments: args,
                            });
                        }
                    }
                }

                // Vertex AI fallback: tool call in finishMessage when no functionDeclarations
                // Format: "Unexpected tool call: {\"tool_name\": \"bash\", \"command\": \"...\"}"
                if calls.is_empty() {
                    if let Some(msg) = candidate.get("finishMessage").and_then(|m| m.as_str()) {
                        tracing::debug!(finish_message = msg, "parsing finishMessage for tool calls");
                        if let Some(json_start) = msg.find('{') {
                            let json_str = &msg[json_start..];
                            // Try to find matching closing brace for clean extraction
                            let json_str = if let Some(end) = json_str.rfind('}') {
                                &json_str[..=end]
                            } else {
                                json_str
                            };
                            if let Ok(tc_json) = serde_json::from_str::<Value>(json_str) {
                                // Handle two formats:
                                // 1. Flat: {"tool_name": "bash", "command": "..."}
                                // 2. Actions array: {"actions": [{"tool_name": "read_file", "path": "..."}]}
                                let items: Vec<Value> = if let Some(actions) = tc_json.get("actions").and_then(|a| a.as_array()) {
                                    actions.clone()
                                } else {
                                    vec![tc_json]
                                };
                                for item in items {
                                    let name = item
                                        .get("tool_name")
                                        .and_then(|n| n.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();
                                    let mut args = item.clone();
                                    if let Some(obj) = args.as_object_mut() {
                                        obj.remove("tool_name");
                                    }
                                    calls.push(ToolCall {
                                        id: name.clone(),
                                        name,
                                        arguments: args,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        calls
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct TestResponse {
        answer: String,
        confidence: f64,
    }

    #[test]
    fn builds_request_with_tools_no_json_mode() {
        let client = GeminiClient::from_api_key("test-key", "gemini-2.5-flash");
        let messages = vec![
            Message::system("You are a helper."),
            Message::user("Hello"),
        ];
        let tools = vec![crate::tool::tool::<TestResponse>("test_tool", "A test")];

        let body = client.build_request::<TestResponse>(&messages, &tools).unwrap();

        // When tools are present, no JSON mode (Gemini doesn't support both)
        assert!(body["generationConfig"]["responseSchema"].is_null());
        assert!(body["generationConfig"]["responseMimeType"].is_null());

        // Has tools + toolConfig
        assert!(body["tools"][0]["functionDeclarations"].is_array());
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");

        // Has system instruction
        assert!(body["systemInstruction"]["parts"][0]["text"].is_string());

        // Contents only has user (system extracted)
        let contents = body["contents"].as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
    }

    #[test]
    fn builds_request_without_tools_has_json_mode() {
        let client = GeminiClient::from_api_key("test-key", "gemini-2.5-flash");
        let messages = vec![Message::user("Hello")];

        let body = client.build_request::<TestResponse>(&messages, &[]).unwrap();

        // Without tools, JSON mode is enabled
        assert!(body["generationConfig"]["responseSchema"].is_object());
        assert_eq!(body["generationConfig"]["responseMimeType"], "application/json");
        assert!(body["tools"].is_null());
    }

    #[test]
    fn parses_text_response() {
        let client = GeminiClient::from_api_key("test", "test");
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "text": "{\"answer\": \"42\", \"confidence\": 0.95}"
                    }]
                }
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 20,
                "totalTokenCount": 30,
            }
        });

        let result: SgrResponse<TestResponse> = client.parse_response(&body, None).unwrap();
        let output = result.output.unwrap();
        assert_eq!(output.answer, "42");
        assert_eq!(output.confidence, 0.95);
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.usage.unwrap().total_tokens, 30);
    }

    #[test]
    fn parses_function_call_response() {
        let client = GeminiClient::from_api_key("test", "test");
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [{
                        "functionCall": {
                            "name": "test_tool",
                            "args": {"input": "/video.mp4"}
                        }
                    }]
                }
            }]
        });

        let result: SgrResponse<TestResponse> = client.parse_response(&body, None).unwrap();
        assert!(result.output.is_none());
        assert_eq!(result.tool_calls.len(), 1);
        assert_eq!(result.tool_calls[0].name, "test_tool");
        assert_eq!(result.tool_calls[0].arguments["input"], "/video.mp4");
        // ID should be unique, not just the tool name
        assert_eq!(result.tool_calls[0].id, "call#test_tool#1");
    }

    #[test]
    fn multiple_function_calls_get_unique_ids() {
        let client = GeminiClient::from_api_key("test", "test");
        let body = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        {"functionCall": {"name": "read_file", "args": {"path": "a.rs"}}},
                        {"functionCall": {"name": "read_file", "args": {"path": "b.rs"}}},
                        {"functionCall": {"name": "write_file", "args": {"path": "c.rs"}}},
                    ]
                }
            }]
        });

        let result: SgrResponse<TestResponse> = client.parse_response(&body, None).unwrap();
        assert_eq!(result.tool_calls.len(), 3);
        assert_eq!(result.tool_calls[0].id, "call#read_file#1");
        assert_eq!(result.tool_calls[1].id, "call#read_file#2");
        assert_eq!(result.tool_calls[2].id, "call#write_file#3");
        // All IDs unique
        let ids: std::collections::HashSet<_> = result.tool_calls.iter().map(|tc| &tc.id).collect();
        assert_eq!(ids.len(), 3);
    }

    #[test]
    fn func_name_extraction_from_call_id() {
        let client = GeminiClient::from_api_key("test", "test");

        // Build messages with tool results using our call ID format
        let messages = vec![
            Message::user("test"),
            Message::tool("call#write_file#1", "Wrote file"),
            Message::tool("call#bash#2", "Output"),
            Message::tool("call#my_custom_tool#10", "Result"),
            Message::tool("old_format_id", "Legacy"),  // fallback
        ];

        let contents = client.messages_to_contents(&messages);
        // Index 0 = user, 1-4 = tool results
        let fr1 = &contents[1]["parts"][0]["functionResponse"];
        assert_eq!(fr1["name"], "write_file");
        let fr2 = &contents[2]["parts"][0]["functionResponse"];
        assert_eq!(fr2["name"], "bash");
        let fr3 = &contents[3]["parts"][0]["functionResponse"];
        assert_eq!(fr3["name"], "my_custom_tool");
        // Fallback: old format without call# prefix
        let fr4 = &contents[4]["parts"][0]["functionResponse"];
        assert_eq!(fr4["name"], "old_format_id");
    }
}
