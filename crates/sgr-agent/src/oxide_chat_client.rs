//! OxideChatClient — LlmClient via Chat Completions API (not Responses).
//!
//! For OpenAI-compatible endpoints that don't support /responses:
//! Cloudflare AI Gateway compat, OpenRouter, local models, Workers AI.

use crate::client::LlmClient;
use crate::tool::ToolDef;
use crate::types::{LlmConfig, Message, Role, SgrError, ToolCall};
use openai_oxide::OpenAI;
use openai_oxide::config::ClientConfig;
use openai_oxide::types::chat::*;
use serde_json::Value;

/// Record OTEL span for Chat Completions API call via shared telemetry helper.
/// AI-NOTE: OxideChatClient is the primary client for Nemotron (Cloudflare Workers AI).
#[cfg(feature = "telemetry")]
fn record_chat_otel(
    model: &str,
    messages: &[Message],
    usage: Option<&openai_oxide::types::chat::Usage>,
    tool_calls: &[ToolCall],
    text_output: &str,
) {
    let (pt, ct, cached) = usage
        .map(|u| {
            let pt = u.prompt_tokens.unwrap_or(0);
            let ct = u.completion_tokens.unwrap_or(0);
            let cached = u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0);
            (pt, ct, cached)
        })
        .unwrap_or((0, 0, 0));

    let input = last_user_content(messages, 500);
    let output = truncate_str(text_output, 500);
    let tc: Vec<(String, String)> = tool_calls
        .iter()
        .map(|tc| (tc.name.clone(), tc.arguments.to_string()))
        .collect();

    crate::telemetry::record_llm_span(
        "chat.completions.api",
        model,
        &input,
        &output,
        &tc,
        &crate::telemetry::LlmUsage {
            prompt_tokens: pt,
            completion_tokens: ct,
            cached_tokens: cached,
            response_model: model.to_string(),
        },
    );
}

#[cfg(not(feature = "telemetry"))]
fn record_chat_otel(
    _model: &str,
    _messages: &[Message],
    _usage: Option<&openai_oxide::types::chat::Usage>,
    _tool_calls: &[ToolCall],
    _text: &str,
) {
}

#[cfg(feature = "telemetry")]
fn last_user_content(messages: &[Message], max_len: usize) -> String {
    messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, Role::User | Role::Tool))
        .map(|m| truncate_str(&m.content, max_len))
        .unwrap_or_default()
}

#[cfg(feature = "telemetry")]
fn truncate_str(s: &str, max_len: usize) -> String {
    use crate::str_ext::StrExt;
    let t = s.trunc(max_len);
    if t.len() < s.len() {
        format!("{t}...")
    } else {
        s.to_string()
    }
}

/// LlmClient backed by openai-oxide Chat Completions API.
pub struct OxideChatClient {
    client: OpenAI,
    pub(crate) model: String,
    pub(crate) temperature: Option<f64>,
    pub(crate) max_tokens: Option<u32>,
    /// Reasoning effort — None disables reasoning for FC (DeepInfra Nemotron Super).
    pub(crate) reasoning_effort: Option<openai_oxide::types::chat::ReasoningEffort>,
    /// Server-side prompt prefix caching key (DeepInfra, OpenAI).
    pub(crate) prompt_cache_key: Option<String>,
}

impl OxideChatClient {
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
            return Err(SgrError::Schema("No API key for oxide chat client".into()));
        }

        let mut client_config = ClientConfig::new(&api_key);
        if let Some(ref url) = config.base_url {
            client_config = client_config.base_url(url.clone());
        }
        config.apply_headers(&mut client_config);

        let reasoning_effort = config.reasoning_effort.as_deref().and_then(|s| match s {
            "none" => Some(openai_oxide::types::chat::ReasoningEffort::None),
            "low" => Some(openai_oxide::types::chat::ReasoningEffort::Low),
            "medium" => Some(openai_oxide::types::chat::ReasoningEffort::Medium),
            "high" => Some(openai_oxide::types::chat::ReasoningEffort::High),
            _ => None,
        });

        Ok(Self {
            client: OpenAI::with_config(client_config),
            model: config.model.clone(),
            temperature: Some(config.temp),
            max_tokens: config.max_tokens,
            reasoning_effort,
            prompt_cache_key: config.prompt_cache_key.clone(),
        })
    }

    fn build_messages(&self, messages: &[Message]) -> Vec<ChatCompletionMessageParam> {
        messages
            .iter()
            .map(|m| match m.role {
                Role::System => ChatCompletionMessageParam::System {
                    content: m.content.clone(),
                    name: None,
                },
                Role::User => ChatCompletionMessageParam::User {
                    content: UserContent::Text(m.content.clone()),
                    name: None,
                },
                Role::Assistant => {
                    let tc = if m.tool_calls.is_empty() {
                        None
                    } else {
                        Some(
                            m.tool_calls
                                .iter()
                                .map(|tc| openai_oxide::types::chat::ToolCall {
                                    id: tc.id.clone(),
                                    type_: "function".into(),
                                    function: openai_oxide::types::chat::FunctionCall {
                                        name: tc.name.clone(),
                                        arguments: tc.arguments.to_string(),
                                    },
                                })
                                .collect(),
                        )
                    };
                    ChatCompletionMessageParam::Assistant {
                        content: if m.content.is_empty() {
                            None
                        } else {
                            Some(m.content.clone())
                        },
                        name: None,
                        tool_calls: tc,
                        refusal: None,
                    }
                }
                Role::Tool => ChatCompletionMessageParam::Tool {
                    content: m.content.clone(),
                    tool_call_id: m.tool_call_id.clone().unwrap_or_default(),
                },
            })
            .collect()
    }

    fn build_request(&self, messages: &[Message]) -> ChatCompletionRequest {
        self.build_request_with_reasoning(messages, self.reasoning_effort.as_ref())
    }

    fn build_request_no_reasoning(&self, messages: &[Message]) -> ChatCompletionRequest {
        // Force reasoning off for action/tool execution calls (faster + cache friendly)
        if self.reasoning_effort.is_some() {
            self.build_request_with_reasoning(
                messages,
                Some(&openai_oxide::types::chat::ReasoningEffort::None),
            )
        } else {
            self.build_request_with_reasoning(messages, None)
        }
    }

    fn build_request_with_reasoning(
        &self,
        messages: &[Message],
        reasoning: Option<&openai_oxide::types::chat::ReasoningEffort>,
    ) -> ChatCompletionRequest {
        let mut req = ChatCompletionRequest::new(&self.model, self.build_messages(messages));
        if let Some(temp) = self.temperature {
            req.temperature = Some(temp);
        }
        if let Some(max) = self.max_tokens {
            if self.model.starts_with("gpt-5") || self.model.starts_with("o") {
                req = req.max_completion_tokens(max as i64);
            } else {
                req.max_tokens = Some(max as i64);
            }
        }
        if let Some(effort) = reasoning {
            req.reasoning_effort = Some(effort.clone());
        }
        if let Some(ref key) = self.prompt_cache_key {
            req.prompt_cache_key = Some(key.clone());
        }
        // Anthropic prompt caching via OpenRouter: top-level cache_control
        if self.model.contains("anthropic/") || self.model.contains("claude") {
            req.cache_control = Some(serde_json::json!({"type": "ephemeral"}));
        }
        req
    }

    fn extract_tool_calls(response: &ChatCompletionResponse) -> Vec<ToolCall> {
        let Some(choice) = response.choices.first() else {
            return Vec::new();
        };
        let Some(ref calls) = choice.message.tool_calls else {
            return Vec::new();
        };
        calls
            .iter()
            .map(|tc| ToolCall {
                id: tc.id.clone(),
                name: tc.function.name.clone(),
                arguments: crate::str_ext::parse_tool_args(&tc.function.arguments),
            })
            .collect()
    }
}

#[async_trait::async_trait]
impl LlmClient for OxideChatClient {
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
        // Skip ensure_strict for pre-strict schemas (e.g., from build_action_schema)
        let strict_schema =
            if schema.get("additionalProperties").and_then(|v| v.as_bool()) == Some(false) {
                schema.clone()
            } else {
                let mut s = schema.clone();
                openai_oxide::parsing::ensure_strict(&mut s);
                s
            };

        let mut req = self.build_request(messages);
        req.response_format = Some(ResponseFormat::JsonSchema {
            json_schema: JsonSchema {
                name: "response".into(),
                description: None,
                schema: Some(strict_schema),
                strict: Some(true),
            },
        });

        let response = self
            .client
            .chat()
            .completions()
            .create(req)
            .await
            .map_err(|e| SgrError::Api {
                status: 0,
                body: e.to_string(),
            })?;

        let raw_text = response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();
        let tool_calls = Self::extract_tool_calls(&response);
        let parsed = serde_json::from_str::<Value>(&raw_text).ok();

        if let Some(ref usage) = response.usage {
            let input = usage.prompt_tokens.unwrap_or(0);
            let cached = usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0);
            let output = usage.completion_tokens.unwrap_or(0);
            if cached > 0 {
                let pct = if input > 0 { cached * 100 / input } else { 0 };
                eprintln!(
                    "    💰 {}in/{}out (cached: {}, {}%)",
                    input, output, cached, pct
                );
            } else {
                eprintln!("    💰 {}in/{}out", input, output);
            }
        }

        record_chat_otel(
            &self.model,
            messages,
            response.usage.as_ref(),
            &tool_calls,
            &raw_text,
        );
        Ok((parsed, tool_calls, raw_text))
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        // No reasoning for tool execution — faster + better cache hit
        let mut req = self.build_request_no_reasoning(messages);

        let chat_tools: Vec<Tool> = tools
            .iter()
            .map(|t| {
                Tool::function(
                    &t.name,
                    if t.description.is_empty() {
                        "No description"
                    } else {
                        &t.description
                    },
                    t.parameters.clone(),
                )
            })
            .collect();
        req.tools = Some(chat_tools);
        req.tool_choice = Some(openai_oxide::types::chat::ToolChoice::Mode(
            "required".into(),
        ));

        let response = self
            .client
            .chat()
            .completions()
            .create(req)
            .await
            .map_err(|e| SgrError::Api {
                status: 0,
                body: e.to_string(),
            })?;

        if let Some(ref usage) = response.usage {
            let input = usage.prompt_tokens.unwrap_or(0);
            let cached = usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0);
            let output = usage.completion_tokens.unwrap_or(0);
            if cached > 0 {
                let pct = if input > 0 { cached * 100 / input } else { 0 };
                eprintln!(
                    "    💰 {}in/{}out (cached: {}, {}%)",
                    input, output, cached, pct
                );
            } else {
                eprintln!("    💰 {}in/{}out", input, output);
            }
        }

        let calls = Self::extract_tool_calls(&response);
        record_chat_otel(&self.model, messages, response.usage.as_ref(), &calls, "");
        // Don't synthesize finish — empty tool_calls signals completion to ToolCallingAgent.
        Ok(calls)
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        let req = self.build_request(messages);

        let response = self
            .client
            .chat()
            .completions()
            .create(req)
            .await
            .map_err(|e| SgrError::Api {
                status: 0,
                body: e.to_string(),
            })?;

        tracing::info!(model = %response.model, "oxide_chat.complete");

        let text = response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();
        record_chat_otel(&self.model, messages, response.usage.as_ref(), &[], &text);
        Ok(text)
    }
}
