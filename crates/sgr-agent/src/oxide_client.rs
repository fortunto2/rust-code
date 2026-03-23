//! OxideClient — LlmClient adapter for `openai-oxide` crate.
//!
//! Uses the **Responses API** (`POST /responses`) instead of Chat Completions.
//! With `oxide-ws` feature: persistent WebSocket connection for -20-25% latency.
//! Supports: structured output (json_schema), function calling, multi-turn (previous_response_id).

use crate::client::LlmClient;
use crate::tool::ToolDef;
use crate::types::{LlmConfig, Message, Role, SgrError, ToolCall};
use openai_oxide::OpenAI;
use openai_oxide::config::ClientConfig;
use openai_oxide::types::responses::*;
use serde_json::Value;

/// Record OTEL attributes on the current span for Phoenix/OpenInference.
#[cfg(feature = "telemetry")]
fn record_otel_usage(response: &Response, model: &str) {
    use opentelemetry::trace::{Span, TraceContextExt, Tracer, TracerProvider};

    let provider = opentelemetry::global::tracer_provider();
    let tracer = provider.tracer("sgr-agent");
    let mut otel_span = tracer.start("oxide.responses.api");

    let pt = response
        .usage
        .as_ref()
        .and_then(|u| u.input_tokens)
        .unwrap_or(0);
    let ct = response
        .usage
        .as_ref()
        .and_then(|u| u.output_tokens)
        .unwrap_or(0);

    // OpenInference conventions (Phoenix)
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "openinference.span.kind",
        "LLM",
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "llm.model_name",
        model.to_string(),
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new("llm.token_count.prompt", pt));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "llm.token_count.completion",
        ct,
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "llm.token_count.total",
        pt + ct,
    ));

    // GenAI conventions (LangSmith)
    otel_span.set_attribute(opentelemetry::KeyValue::new("langsmith.span.kind", "LLM"));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.request.model",
        model.to_string(),
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.response.model",
        response.model.clone(),
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.usage.prompt_tokens",
        pt,
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.usage.completion_tokens",
        ct,
    ));

    // Output text
    let output = response.output_text();
    if !output.is_empty() {
        otel_span.set_attribute(opentelemetry::KeyValue::new(
            "gen_ai.completion.0.content",
            if output.len() > 4000 {
                format!("{}...", &output[..4000])
            } else {
                output
            },
        ));
    }

    otel_span.end();
}

#[cfg(not(feature = "telemetry"))]
fn record_otel_usage(_response: &Response, _model: &str) {}

/// LlmClient backed by openai-oxide (Responses API).
///
/// With `oxide-ws` feature: call `connect_ws()` to upgrade to WebSocket mode.
/// All subsequent calls go over persistent wss:// connection (-20-25% latency).
pub struct OxideClient {
    client: OpenAI,
    pub(crate) model: String,
    pub(crate) temperature: Option<f64>,
    pub(crate) max_tokens: Option<u32>,
    /// Last response_id for multi-turn chaining.
    last_response_id: std::sync::Mutex<Option<String>>,
    /// WebSocket session (when oxide-ws feature is enabled and connected).
    #[cfg(feature = "oxide-ws")]
    ws: tokio::sync::Mutex<Option<openai_oxide::websocket::WsSession>>,
    /// Lazy WS: true = connect on first request, false = HTTP only.
    #[cfg(feature = "oxide-ws")]
    ws_enabled: std::sync::atomic::AtomicBool,
}

impl OxideClient {
    /// Create from LlmConfig.
    pub fn from_config(config: &LlmConfig) -> Result<Self, SgrError> {
        let api_key = config
            .api_key
            .clone()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .unwrap_or_else(|| {
                if config.base_url.is_some() {
                    "dummy_key".into()
                } else {
                    "".into()
                }
            });

        if api_key.is_empty() {
            return Err(SgrError::Schema("No API key for oxide client".into()));
        }

        let mut client_config = ClientConfig::new(&api_key);
        if let Some(ref url) = config.base_url {
            client_config = client_config.base_url(url.clone());
        }

        Ok(Self {
            client: OpenAI::with_config(client_config),
            model: config.model.clone(),
            temperature: Some(config.temp),
            max_tokens: config.max_tokens,
            last_response_id: std::sync::Mutex::new(None),
            #[cfg(feature = "oxide-ws")]
            ws: tokio::sync::Mutex::new(None),
            #[cfg(feature = "oxide-ws")]
            ws_enabled: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Enable WebSocket mode — lazy connect on first request.
    ///
    /// Does NOT open a connection immediately. The WS connection is established
    /// on the first `send_request_auto()` call, eliminating idle timeout issues.
    /// Falls back to HTTP automatically if WS fails.
    ///
    /// Requires `oxide-ws` feature.
    #[cfg(feature = "oxide-ws")]
    pub async fn connect_ws(&self) -> Result<(), SgrError> {
        self.ws_enabled
            .store(true, std::sync::atomic::Ordering::Relaxed);
        tracing::info!(model = %self.model, "oxide WebSocket enabled (lazy connect)");
        Ok(())
    }

    /// Send request — lazy WS connect + send, falls back to HTTP on any WS error.
    async fn send_request_auto(
        &self,
        request: ResponseCreateRequest,
    ) -> Result<Response, SgrError> {
        #[cfg(feature = "oxide-ws")]
        if self.ws_enabled.load(std::sync::atomic::Ordering::Relaxed) {
            let mut ws_guard = self.ws.lock().await;

            // Lazy connect
            if ws_guard.is_none() {
                match self.client.ws_session().await {
                    Ok(session) => {
                        tracing::info!(model = %self.model, "oxide WS connected (lazy)");
                        *ws_guard = Some(session);
                    }
                    Err(e) => {
                        tracing::warn!("oxide WS connect failed, using HTTP: {e}");
                        self.ws_enabled
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }

            if let Some(ref mut session) = *ws_guard {
                match session.send(request.clone()).await {
                    Ok(response) => return Ok(response),
                    Err(e) => {
                        tracing::warn!("oxide WS send failed, falling back to HTTP: {e}");
                        *ws_guard = None;
                    }
                }
            }
        }

        // HTTP fallback
        self.client
            .responses()
            .create(request)
            .await
            .map_err(|e| SgrError::Api {
                status: 0,
                body: e.to_string(),
            })
    }

    /// Build request with mixed input: regular messages + function_call_output items.
    /// Required when chaining with previous_response_id after a function call response.
    fn build_request_with_tool_outputs(&self, messages: &[Message]) -> ResponseCreateRequest {
        use openai_oxide::types::responses::ResponseInput;

        let mut items: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::Tool => {
                    if let Some(ref call_id) = msg.tool_call_id {
                        // Responses API function_call_output item
                        items.push(serde_json::json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": msg.content
                        }));
                    }
                }
                Role::System => {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "system",
                        "content": msg.content
                    }));
                }
                Role::User => {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": msg.content
                    }));
                }
                Role::Assistant => {
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "assistant",
                        "content": msg.content
                    }));
                }
            }
        }

        let mut req = ResponseCreateRequest::new(&self.model);
        if !items.is_empty() {
            req.input = Some(ResponseInput::Items(items));
        }

        // Temperature: send normally. openai-oxide WS layer auto-strips decimal values
        // (OpenAI WS bug: https://community.openai.com/t/1375536).
        if let Some(temp) = self.temperature {
            if (temp - 1.0).abs() > f64::EPSILON {
                req = req.temperature(temp);
            }
        }
        if let Some(max) = self.max_tokens {
            req = req.max_output_tokens(max as i64);
        }

        // Chain previous response if available
        if let Some(prev_id) = self.last_response_id.lock().ok().and_then(|g| g.clone()) {
            req = req.previous_response_id(prev_id);
        }

        req
    }

    /// Build a ResponseCreateRequest from messages + optional schema.
    fn build_request(&self, messages: &[Message], schema: Option<&Value>) -> ResponseCreateRequest {
        let mut input_items = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    input_items.push(ResponseInputItem {
                        role: openai_oxide::types::common::Role::System,
                        content: Value::String(msg.content.clone()),
                    });
                }
                Role::User => {
                    input_items.push(ResponseInputItem {
                        role: openai_oxide::types::common::Role::User,
                        content: Value::String(msg.content.clone()),
                    });
                }
                Role::Assistant => {
                    input_items.push(ResponseInputItem {
                        role: openai_oxide::types::common::Role::Assistant,
                        content: Value::String(msg.content.clone()),
                    });
                }
                Role::Tool => {
                    let tool_result = if let Some(ref id) = msg.tool_call_id {
                        format!("[Tool result for {}]: {}", id, msg.content)
                    } else {
                        msg.content.clone()
                    };
                    input_items.push(ResponseInputItem {
                        role: openai_oxide::types::common::Role::User,
                        content: Value::String(tool_result),
                    });
                }
            }
        }

        let mut req = ResponseCreateRequest::new(&self.model);

        // Set input — prefer simple text when single user message (fewer tokens)
        if input_items.len() == 1 && input_items[0].role == openai_oxide::types::common::Role::User
        {
            if let Some(text) = input_items[0].content.as_str() {
                req = req.input(text);
            } else {
                req.input = Some(ResponseInput::Messages(input_items));
            }
        } else if !input_items.is_empty() {
            req.input = Some(ResponseInput::Messages(input_items));
        }

        // Temperature — skip default to reduce payload
        if let Some(temp) = self.temperature {
            if (temp - 1.0).abs() > f64::EPSILON {
                req = req.temperature(temp);
            }
        }

        // Max tokens
        if let Some(max) = self.max_tokens {
            req = req.max_output_tokens(max as i64);
        }

        // Structured output via json_schema
        if let Some(schema_val) = schema {
            req = req.text(ResponseTextConfig {
                format: Some(ResponseTextFormat::JsonSchema {
                    name: "sgr_response".into(),
                    description: None,
                    schema: Some(schema_val.clone()),
                    strict: Some(true),
                }),
                verbosity: None,
            });
        }

        // Chain previous response if available
        if let Some(prev_id) = self.last_response_id.lock().ok().and_then(|g| g.clone()) {
            req = req.previous_response_id(prev_id);
        }

        req
    }

    /// Save response_id for multi-turn chaining.
    fn save_response_id(&self, id: &str) {
        if let Ok(mut guard) = self.last_response_id.lock() {
            *guard = Some(id.to_string());
        }
    }

    /// Set response_id externally (for stateful session coordination with coach).
    pub fn set_response_id(&self, id: Option<&str>) {
        if let Ok(mut guard) = self.last_response_id.lock() {
            *guard = id.map(String::from);
        }
    }

    /// Get current response_id.
    pub fn response_id(&self) -> Option<String> {
        self.last_response_id.lock().ok().and_then(|g| g.clone())
    }

    /// Function calling with explicit previous_response_id.
    /// Returns tool calls + new response_id for chaining.
    ///
    /// Always sets `store(true)` so responses can be referenced by subsequent calls.
    /// When `previous_response_id` is provided, only delta messages need to be sent
    /// (server has full history from previous stored response).
    ///
    /// Tool messages (role=Tool with tool_call_id) are converted to Responses API
    /// `function_call_output` items — required for chaining with previous_response_id.
    pub async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        // Set external response_id for chaining
        if let Some(pid) = previous_response_id {
            self.set_response_id(Some(pid));
        }

        // Always use Items format (with "type":"message" on each item).
        // HTTP API accepts Messages format (without type), but WS API requires it.
        // Using Items consistently ensures both HTTP and WS work.
        let mut req = self.build_request_with_tool_outputs(messages);
        // Always store so next call can chain via previous_response_id
        req = req.store(true);

        // Convert ToolDefs to ResponseTools with strict mode.
        // strict: true guarantees LLM output matches schema exactly (no parse errors).
        // oxide ensure_strict() handles: additionalProperties, all-required,
        // nullable→anyOf, allOf inlining, oneOf→anyOf.
        let response_tools: Vec<ResponseTool> = tools
            .iter()
            .map(|t| {
                let mut params = t.parameters.clone();
                openai_oxide::parsing::ensure_strict(&mut params);
                ResponseTool::Function {
                    name: t.name.clone(),
                    description: if t.description.is_empty() {
                        None
                    } else {
                        Some(t.description.clone())
                    },
                    parameters: Some(params),
                    strict: Some(true),
                }
            })
            .collect();
        req = req.tools(response_tools);

        let response = self.send_request_auto(req).await?;

        let response_id = response.id.clone();
        self.save_response_id(&response_id);
        record_otel_usage(&response, &self.model);

        let input_tokens = response
            .usage
            .as_ref()
            .and_then(|u| u.input_tokens)
            .unwrap_or(0);
        let cached_tokens = response
            .usage
            .as_ref()
            .and_then(|u| u.input_tokens_details.as_ref())
            .and_then(|d| d.cached_tokens)
            .unwrap_or(0);

        tracing::info!(
            model = %response.model,
            response_id = %response_id,
            input_tokens,
            cached_tokens,
            chained = previous_response_id.is_some(),
            "oxide.tools_call_stateful"
        );

        Ok((Self::extract_tool_calls(&response), Some(response_id)))
    }

    /// Extract tool calls from Responses API output items.
    fn extract_tool_calls(response: &Response) -> Vec<ToolCall> {
        response
            .function_calls()
            .into_iter()
            .map(|fc| ToolCall {
                id: fc.call_id,
                name: fc.name,
                arguments: fc.arguments,
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl LlmClient for OxideClient {
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
        // Make schema OpenAI-strict (oxide handles nullable, allOf, etc.)
        let mut strict_schema = schema.clone();
        openai_oxide::parsing::ensure_strict(&mut strict_schema);

        let req = self.build_request(messages, Some(&strict_schema));

        let span = tracing::info_span!(
            "oxide.responses.create",
            model = %self.model,
            method = "structured_call",
        );
        let _enter = span.enter();

        let response = self.send_request_auto(req).await?;

        self.save_response_id(&response.id);
        record_otel_usage(&response, &self.model);

        let raw_text = response.output_text();
        let tool_calls = Self::extract_tool_calls(&response);
        let parsed = serde_json::from_str::<Value>(&raw_text).ok();

        tracing::info!(
            model = %response.model,
            response_id = %response.id,
            input_tokens = response.usage.as_ref().and_then(|u| u.input_tokens).unwrap_or(0),
            output_tokens = response.usage.as_ref().and_then(|u| u.output_tokens).unwrap_or(0),
            "oxide.structured_call"
        );

        Ok((parsed, tool_calls, raw_text))
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        let mut req = self.build_request(messages, None);

        // Convert ToolDefs to ResponseTools — no strict mode (faster server-side)
        let response_tools: Vec<ResponseTool> = tools
            .iter()
            .map(|t| ResponseTool::Function {
                name: t.name.clone(),
                description: if t.description.is_empty() {
                    None
                } else {
                    Some(t.description.clone())
                },
                parameters: Some(t.parameters.clone()),
                strict: None,
            })
            .collect();
        req = req.tools(response_tools);

        let response = self.send_request_auto(req).await?;

        self.save_response_id(&response.id);
        record_otel_usage(&response, &self.model);

        tracing::info!(
            model = %response.model,
            response_id = %response.id,
            "oxide.tools_call"
        );

        Ok(Self::extract_tool_calls(&response))
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        let req = self.build_request(messages, None);

        let response = self.send_request_auto(req).await?;

        self.save_response_id(&response.id);
        record_otel_usage(&response, &self.model);

        let text = response.output_text();
        if text.is_empty() {
            return Err(SgrError::EmptyResponse);
        }

        tracing::info!(
            model = %response.model,
            response_id = %response.id,
            input_tokens = response.usage.as_ref().and_then(|u| u.input_tokens).unwrap_or(0),
            output_tokens = response.usage.as_ref().and_then(|u| u.output_tokens).unwrap_or(0),
            "oxide.complete"
        );

        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oxide_client_from_config() {
        // Just test construction doesn't panic
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = OxideClient::from_config(&config).unwrap();
        assert_eq!(client.model, "gpt-5.4");
    }

    #[test]
    fn build_request_simple() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4").temperature(0.5);
        let client = OxideClient::from_config(&config).unwrap();
        let messages = vec![Message::system("Be helpful."), Message::user("Hello")];
        let req = client.build_request(&messages, None);
        assert_eq!(req.model, "gpt-5.4");
        // System prompt goes as input message (not instructions) for fewer tokens
        assert!(req.instructions.is_none());
        assert!(req.input.is_some()); // system + user as messages
        assert_eq!(req.temperature, Some(0.5));
    }

    #[test]
    fn build_request_with_schema() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = OxideClient::from_config(&config).unwrap();
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"]
        });
        let req = client.build_request(&[Message::user("Hi")], Some(&schema));
        assert!(req.text.is_some());
    }
}
