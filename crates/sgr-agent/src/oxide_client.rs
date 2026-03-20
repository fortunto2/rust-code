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
}

impl OxideClient {
    /// Create from LlmConfig.
    pub fn from_config(config: &LlmConfig) -> Result<Self, SgrError> {
        let api_key = config
            .api_key
            .clone()
            .or_else(|| std::env::var("OPENAI_API_KEY").ok())
            .ok_or_else(|| SgrError::Schema("No API key for oxide client".into()))?;

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
        })
    }

    /// Upgrade to WebSocket mode for lower latency.
    ///
    /// Opens a persistent `wss://` connection. All subsequent calls go through
    /// the WebSocket instead of HTTP, saving ~200ms per request.
    ///
    /// Requires `oxide-ws` feature.
    #[cfg(feature = "oxide-ws")]
    pub async fn connect_ws(&self) -> Result<(), SgrError> {
        let session = self.client.ws_session().await.map_err(|e| SgrError::Api {
            status: 0,
            body: format!("WebSocket connect: {e}"),
        })?;
        *self.ws.lock().await = Some(session);
        tracing::info!(model = %self.model, "oxide WebSocket connected");
        Ok(())
    }

    /// Send request — uses WebSocket if connected, otherwise HTTP.
    async fn send_request_auto(
        &self,
        request: ResponseCreateRequest,
    ) -> Result<Response, SgrError> {
        #[cfg(feature = "oxide-ws")]
        {
            let mut ws_guard = self.ws.lock().await;
            if let Some(ref mut session) = *ws_guard {
                return session.send(request).await.map_err(|e| SgrError::Api {
                    status: 0,
                    body: e.to_string(),
                });
            }
        }

        // Fallback to HTTP
        self.client
            .responses()
            .create(request)
            .await
            .map_err(|e| SgrError::Api {
                status: 0,
                body: e.to_string(),
            })
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

        // Chain previous response if available — only store when chaining
        if let Some(prev_id) = self.last_response_id.lock().ok().and_then(|g| g.clone()) {
            req = req.previous_response_id(prev_id).store(true);
        }

        req
    }

    /// Save response_id for multi-turn chaining.
    fn save_response_id(&self, id: &str) {
        if let Ok(mut guard) = self.last_response_id.lock() {
            *guard = Some(id.to_string());
        }
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
        // Make schema OpenAI-strict
        let mut strict_schema = schema.clone();
        crate::schema::make_openai_strict(&mut strict_schema);

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
        assert_eq!(req.instructions.as_deref(), Some("Be helpful."));
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
