//! AsyncOpenAIClient — LlmClient adapter for `async-openai` crate.
//!
//! Uses the **Responses API** (`POST /responses`) instead of Chat Completions.
//! Supports: structured output (json_schema), function calling, multi-turn (previous_response_id).

use crate::client::LlmClient;
use crate::tool::ToolDef;
use crate::types::{LlmConfig, Message, Role, SgrError, ToolCall};
use async_openai::Client;
use async_openai::config::OpenAIConfig;
use async_openai::types::responses::*;
use serde_json::Value;

/// Record OTEL attributes on the current span for Phoenix/OpenInference.
#[cfg(feature = "telemetry")]
fn record_otel_usage(response: &Response, model: &str) {
    use opentelemetry::trace::{Span, Tracer, TracerProvider};

    let provider = opentelemetry::global::tracer_provider();
    let tracer = provider.tracer("sgr-agent");
    let mut otel_span = tracer.start("async_openai.responses.api");

    let pt = response.usage.as_ref().map(|u| u.input_tokens).unwrap_or(0);
    let ct = response
        .usage
        .as_ref()
        .map(|u| u.output_tokens)
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
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "llm.token_count.prompt",
        i64::from(pt),
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "llm.token_count.completion",
        i64::from(ct),
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "llm.token_count.total",
        i64::from(pt + ct),
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
        i64::from(pt),
    ));
    otel_span.set_attribute(opentelemetry::KeyValue::new(
        "gen_ai.usage.completion_tokens",
        i64::from(ct),
    ));

    // Output text
    let output = response.output_text().unwrap_or_default();
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

/// LlmClient backed by async-openai (Responses API).
pub struct AsyncOpenAIClient {
    client: Client<OpenAIConfig>,
    pub(crate) model: String,
    pub(crate) temperature: Option<f32>,
    pub(crate) max_tokens: Option<u32>,
    /// Last response_id for multi-turn chaining.
    last_response_id: std::sync::Mutex<Option<String>>,
}

impl AsyncOpenAIClient {
    /// Create from LlmConfig.
    pub fn from_config(config: &LlmConfig) -> Result<Self, SgrError> {
        let mut openai_config = OpenAIConfig::new();

        // Set API key (from config or env var)
        if let Some(ref key) = config.api_key {
            openai_config = openai_config.with_api_key(key);
        }
        // If no key in config, OpenAIConfig::new() reads OPENAI_API_KEY env var

        // Set base URL if provided
        if let Some(ref url) = config.base_url {
            openai_config = openai_config.with_api_base(url);
        }

        Ok(Self {
            client: Client::with_config(openai_config),
            model: config.model.clone(),
            temperature: Some(config.temp as f32),
            max_tokens: config.max_tokens,
            last_response_id: std::sync::Mutex::new(None),
        })
    }

    /// Build a CreateResponse from messages + optional schema.
    fn build_request(
        &self,
        messages: &[Message],
        schema: Option<&Value>,
    ) -> Result<CreateResponse, SgrError> {
        // Separate system messages from conversation
        let mut instructions = String::new();
        let mut input_items: Vec<InputItem> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    if !instructions.is_empty() {
                        instructions.push_str("\n\n");
                    }
                    instructions.push_str(&msg.content);
                }
                Role::User => {
                    input_items.push(InputItem::EasyMessage(EasyInputMessage {
                        r#type: MessageType::Message,
                        role: async_openai::types::responses::Role::User,
                        content: EasyInputContent::Text(msg.content.clone()),
                    }));
                }
                Role::Assistant => {
                    input_items.push(InputItem::EasyMessage(EasyInputMessage {
                        r#type: MessageType::Message,
                        role: async_openai::types::responses::Role::Assistant,
                        content: EasyInputContent::Text(msg.content.clone()),
                    }));
                }
                Role::Tool => {
                    // Tool results — append as user message with context
                    let tool_result = if let Some(ref id) = msg.tool_call_id {
                        format!("[Tool result for {}]: {}", id, msg.content)
                    } else {
                        msg.content.clone()
                    };
                    input_items.push(InputItem::EasyMessage(EasyInputMessage {
                        r#type: MessageType::Message,
                        role: async_openai::types::responses::Role::User,
                        content: EasyInputContent::Text(tool_result),
                    }));
                }
            }
        }

        let mut req = CreateResponseArgs::default();
        req.model(self.model.clone());

        // Set input
        if input_items.is_empty() && instructions.is_empty() {
            req.input("");
        } else if input_items.len() == 1 && instructions.is_empty() {
            // Try simple text for single-message case
            if let InputItem::EasyMessage(ref m) = input_items[0] {
                if let EasyInputContent::Text(ref t) = m.content {
                    req.input(t.clone());
                } else {
                    req.input(InputParam::Items(input_items));
                }
            } else {
                req.input(InputParam::Items(input_items));
            }
        } else if !input_items.is_empty() {
            req.input(InputParam::Items(input_items));
        }

        // Instructions (system prompt)
        if !instructions.is_empty() {
            req.instructions(instructions);
        }

        // Temperature
        if let Some(temp) = self.temperature {
            req.temperature(temp);
        }

        // Max tokens
        if let Some(max) = self.max_tokens {
            req.max_output_tokens(max);
        }

        // Structured output via json_schema
        if let Some(schema_val) = schema {
            req.text(ResponseTextParam {
                format: TextResponseFormatConfiguration::JsonSchema(ResponseFormatJsonSchema {
                    name: "sgr_response".into(),
                    description: None,
                    schema: Some(schema_val.clone()),
                    strict: Some(true),
                }),
                verbosity: None,
            });
        }

        // Store for multi-turn
        req.store(true);

        // Chain previous response if available
        if let Ok(guard) = self.last_response_id.lock() {
            if let Some(ref prev_id) = *guard {
                req.previous_response_id(prev_id.clone());
            }
        }

        req.build().map_err(|e| SgrError::Schema(e.to_string()))
    }

    /// Save response_id for multi-turn chaining.
    fn save_response_id(&self, id: &str) {
        if let Ok(mut guard) = self.last_response_id.lock() {
            *guard = Some(id.to_string());
        }
    }

    /// Extract tool calls from Responses API output items.
    fn extract_tool_calls(response: &Response) -> Vec<ToolCall> {
        let mut calls = Vec::new();
        for item in &response.output {
            if let OutputItem::FunctionCall(fc) = item {
                let id = fc.call_id.clone();
                let name = fc.name.clone();
                let arguments = serde_json::from_str::<Value>(&fc.arguments)
                    .unwrap_or(Value::Object(Default::default()));
                calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
        }
        calls
    }
}

#[async_trait::async_trait]
impl LlmClient for AsyncOpenAIClient {
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
        // Make schema OpenAI-strict
        let mut strict_schema = schema.clone();
        crate::schema::make_openai_strict(&mut strict_schema);

        let req = self.build_request(messages, Some(&strict_schema))?;

        let span = tracing::info_span!(
            "async_openai.responses.create",
            model = %self.model,
            method = "structured_call",
        );
        let _enter = span.enter();

        let response = self
            .client
            .responses()
            .create(req)
            .await
            .map_err(|e| SgrError::Api {
                status: 0,
                body: e.to_string(),
            })?;

        self.save_response_id(&response.id);
        record_otel_usage(&response, &self.model);

        let raw_text = response.output_text().unwrap_or_default();
        let tool_calls = Self::extract_tool_calls(&response);
        let parsed = serde_json::from_str::<Value>(&raw_text).ok();

        tracing::info!(
            model = %response.model,
            response_id = %response.id,
            input_tokens = response.usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
            output_tokens = response.usage.as_ref().map(|u| u.output_tokens).unwrap_or(0),
            "async_openai.structured_call"
        );

        Ok((parsed, tool_calls, raw_text))
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        let mut req = self.build_request(messages, None)?;

        // Convert ToolDefs to async-openai Tool::Function
        let response_tools: Vec<Tool> = tools
            .iter()
            .map(|t| {
                Tool::Function(FunctionTool {
                    name: t.name.clone(),
                    description: if t.description.is_empty() {
                        None
                    } else {
                        Some(t.description.clone())
                    },
                    parameters: Some(t.parameters.clone()),
                    strict: Some(true),
                })
            })
            .collect();
        req.tools = Some(response_tools);

        let response = self
            .client
            .responses()
            .create(req)
            .await
            .map_err(|e| SgrError::Api {
                status: 0,
                body: e.to_string(),
            })?;

        self.save_response_id(&response.id);
        record_otel_usage(&response, &self.model);

        tracing::info!(
            model = %response.model,
            response_id = %response.id,
            "async_openai.tools_call"
        );

        Ok(Self::extract_tool_calls(&response))
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        let req = self.build_request(messages, None)?;

        let response = self
            .client
            .responses()
            .create(req)
            .await
            .map_err(|e| SgrError::Api {
                status: 0,
                body: e.to_string(),
            })?;

        self.save_response_id(&response.id);
        record_otel_usage(&response, &self.model);

        let text = response.output_text().unwrap_or_default();
        if text.is_empty() {
            return Err(SgrError::EmptyResponse);
        }

        tracing::info!(
            model = %response.model,
            response_id = %response.id,
            input_tokens = response.usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
            output_tokens = response.usage.as_ref().map(|u| u.output_tokens).unwrap_or(0),
            "async_openai.complete"
        );

        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn async_openai_client_from_config() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = AsyncOpenAIClient::from_config(&config).unwrap();
        assert_eq!(client.model, "gpt-5.4");
    }

    #[test]
    fn build_request_simple() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4").temperature(0.5);
        let client = AsyncOpenAIClient::from_config(&config).unwrap();
        let messages = vec![Message::system("Be helpful."), Message::user("Hello")];
        let req = client.build_request(&messages, None).unwrap();
        assert_eq!(req.model, Some("gpt-5.4".into()));
        assert_eq!(req.instructions, Some("Be helpful.".into()));
        assert_eq!(req.temperature, Some(0.5));
    }

    #[test]
    fn build_request_with_schema() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = AsyncOpenAIClient::from_config(&config).unwrap();
        let schema = serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"]
        });
        let req = client
            .build_request(&[Message::user("Hi")], Some(&schema))
            .unwrap();
        assert!(req.text.is_some());
    }
}
