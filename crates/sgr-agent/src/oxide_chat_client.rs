//! OxideChatClient — LlmClient via Chat Completions API (not Responses).
//!
//! For OpenAI-compatible endpoints that don't support /responses:
//! Cloudflare AI Gateway compat, OpenRouter, local models, Workers AI.

use crate::client::LlmClient;
use crate::multimodal;
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
    /// Session ID for sticky routing and trace grouping.
    pub(crate) session_id: Option<String>,
    /// Prompt cache TTL (resolved from config).
    cache_ttl: Option<String>,
    /// Provider to pin on OpenRouter (resolved from config).
    pin_provider: Option<String>,
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

        let mut client_config =
            ClientConfig::new(&api_key).timeout_secs(crate::http_client::REQUEST_TIMEOUT_SECS);
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
            session_id: config.session_id.clone(),
            cache_ttl: config.resolved_cache_ttl().map(String::from),
            pin_provider: config.resolved_pin_provider().map(String::from),
        })
    }

    fn build_messages(&self, messages: &[Message]) -> Vec<ChatCompletionMessageParam> {
        // A `role:"tool"` message is only valid when a preceding assistant
        // message offered a tool_call with the same id. Session-based loops
        // (app_loop) never record the assistant turn, so their tool results
        // arrive orphaned — strict endpoints 400 on that, and Cloudflare's
        // gemma template silently drops the message, leaving the model blind
        // to every tool output (measured: the agent re-listed the same folder
        // five times into a loop abort). Orphans are sent as user messages.
        let offered_ids: std::collections::HashSet<&str> = messages
            .iter()
            .flat_map(|m| m.tool_calls.iter().map(|tc| tc.id.as_str()))
            .collect();
        let result: Vec<ChatCompletionMessageParam> = messages
            .iter()
            .map(|m| match m.role {
                Role::System => ChatCompletionMessageParam::System {
                    content: m.content.clone(),
                    name: None,
                },
                Role::User => {
                    // Multimodal (text + image) if the caller attached images;
                    // otherwise plain text. `chat_parts` is the same helper the
                    // legacy `OpenAIClient` uses — identical wire shape.
                    let content = if m.images.is_empty() {
                        UserContent::Text(m.content.clone())
                    } else {
                        UserContent::Parts(multimodal::chat_parts(&m.content, &m.images))
                    };
                    ChatCompletionMessageParam::User {
                        content,
                        name: None,
                    }
                }
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
                Role::Tool => {
                    let cid = m.tool_call_id.as_deref().unwrap_or("");
                    if !cid.is_empty() && offered_ids.contains(cid) {
                        ChatCompletionMessageParam::Tool {
                            content: m.content.clone(),
                            tool_call_id: cid.to_string(),
                        }
                    } else {
                        // Orphan tool result — no assistant tool_call to pair
                        // with. As a user message the model actually sees it,
                        // images included (the Tool wire shape carries none).
                        let text = format!("[tool result]\n{}", m.content);
                        let content = if m.images.is_empty() {
                            UserContent::Text(text)
                        } else {
                            UserContent::Parts(multimodal::chat_parts(&text, &m.images))
                        };
                        ChatCompletionMessageParam::User {
                            content,
                            name: None,
                        }
                    }
                }
            })
            .collect();

        result
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
        // Session ID for sticky routing + trace grouping — OpenRouter only.
        // AI-NOTE: OpenAI Chat API rejects unknown param 'session_id'.
        // Only set when OpenRouter features are present (cache_ttl or pin_provider).
        if let Some(ref sid) = self.session_id
            && (self.cache_ttl.is_some() || self.pin_provider.is_some())
        {
            req.session_id = Some(sid.clone());
        }
        // Provider capabilities (resolved from LlmConfig, not string checks)
        if let Some(ref ttl) = self.cache_ttl {
            req.cache_control = Some(serde_json::json!({"type": "ephemeral", "ttl": ttl}));
        }
        if let Some(ref provider) = self.pin_provider
            && let Ok(prefs) =
                openai_oxide::openrouter::ProviderPreferences::pinned(provider).to_value()
        {
            req.provider = Some(prefs);
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

        // AI-NOTE: OpenRouter routes to Azure which enforces OpenAI strict mode on ALL tools.
        // ensure_strict adds additionalProperties:false + all properties in required.
        // Safe for non-strict providers (just adds redundant fields).
        let chat_tools: Vec<Tool> = tools
            .iter()
            .map(|t| {
                let mut params = t.parameters.clone();
                openai_oxide::parsing::ensure_strict(&mut params);
                Tool::function(
                    &t.name,
                    if t.description.is_empty() {
                        "No description"
                    } else {
                        &t.description
                    },
                    params,
                )
            })
            .collect();
        req.tools = Some(chat_tools);
        req.tool_choice = Some(openai_oxide::types::chat::ToolChoice::Mode(
            "required".into(),
        ));
        // AI-NOTE: OpenRouter returns 404 for parallel_tool_calls on Anthropic models.
        // Anthropic uses disable_parallel_tool_use (different API), so skip for anthropic/.
        if !self.model.contains("anthropic/") {
            req.parallel_tool_calls = Some(true);
        }

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

    // AI-NOTE: tools_call_with_text — single-phase agent needs text+tools in one call.
    // tool_choice="auto" so model can return text reasoning alongside tool calls.
    async fn tools_call_with_text(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<(Vec<ToolCall>, String), SgrError> {
        let mut req = self.build_request_no_reasoning(messages);

        let chat_tools: Vec<Tool> = tools
            .iter()
            .map(|t| {
                let mut params = t.parameters.clone();
                openai_oxide::parsing::ensure_strict(&mut params);
                Tool::function(
                    &t.name,
                    if t.description.is_empty() {
                        "No description"
                    } else {
                        &t.description
                    },
                    params,
                )
            })
            .collect();
        req.tools = Some(chat_tools);
        // "auto" not "required" — model can return text + tools or just text
        req.tool_choice = Some(openai_oxide::types::chat::ToolChoice::Mode("auto".into()));

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

        let text = response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();
        let calls = Self::extract_tool_calls(&response);
        record_chat_otel(
            &self.model,
            messages,
            response.usage.as_ref(),
            &calls,
            &text,
        );
        Ok((calls, text))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> OxideChatClient {
        let config = LlmConfig {
            base_url: Some("http://localhost:1".into()),
            model: "test".into(),
            ..Default::default()
        };
        OxideChatClient::from_config(&config).expect("test client")
    }

    #[test]
    fn orphan_tool_message_becomes_user() {
        // No assistant tool_calls in history — the "tool" call_id that
        // Session-based loops stamp pairs with nothing.
        let msgs = vec![
            Message::user("list the folder"),
            Message::tool("tool", "{\"ok\":true,\"entries\":[]}"),
        ];
        let built = client().build_messages(&msgs);
        match &built[1] {
            ChatCompletionMessageParam::User { content, .. } => match content {
                UserContent::Text(t) => {
                    assert!(t.starts_with("[tool result]"), "prefix names the turn: {t}");
                    assert!(t.contains("entries"), "payload survives: {t}");
                }
                _ => panic!("expected text content"),
            },
            other => panic!("orphan tool must be sent as user, got {other:?}"),
        }
    }

    #[test]
    fn paired_tool_message_stays_tool() {
        let mut assistant = Message::assistant("");
        assistant.tool_calls = vec![ToolCall {
            id: "call_1".into(),
            name: "file_list".into(),
            arguments: serde_json::json!({"path": "eval"}),
        }];
        let msgs = vec![
            Message::user("list the folder"),
            assistant,
            Message::tool("call_1", "{\"ok\":true}"),
        ];
        let built = client().build_messages(&msgs);
        match &built[2] {
            ChatCompletionMessageParam::Tool { tool_call_id, .. } => {
                assert_eq!(tool_call_id, "call_1");
            }
            other => panic!("paired tool must keep the tool role, got {other:?}"),
        }
    }
}
