//! OxideClient — LlmClient adapter for `openai-oxide` crate.
//!
//! Uses the **Responses API** (`POST /responses`) instead of Chat Completions.
//! With `oxide-ws` feature: persistent WebSocket connection for -20-25% latency.
//! Supports: structured output (json_schema), function calling, multi-turn (previous_response_id).

use crate::client::LlmClient;
use crate::multimodal;
use crate::tool::ToolDef;
use crate::types::{LlmConfig, Message, Role, SgrError, ToolCall};
use openai_oxide::OpenAI;
use openai_oxide::config::ClientConfig;
use openai_oxide::types::responses::*;
use serde_json::Value;

/// Record OTEL span for Responses API call via shared telemetry helper.
#[cfg(feature = "telemetry")]
fn record_otel_usage(response: &Response, model: &str, messages: &[Message]) {
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
    let cached = response
        .usage
        .as_ref()
        .and_then(|u| u.input_tokens_details.as_ref())
        .and_then(|d| d.cached_tokens)
        .unwrap_or(0);

    let input = last_user_content(messages, 500);
    let output_text = response.output_text();
    let output = truncate_str(&output_text, 500);
    let tool_calls: Vec<(String, String)> = response
        .function_calls()
        .iter()
        .map(|fc| (fc.name.clone(), fc.arguments.to_string()))
        .collect();

    crate::telemetry::record_llm_span(
        "oxide.responses.api",
        model,
        &input,
        &output,
        &tool_calls,
        &crate::telemetry::LlmUsage {
            prompt_tokens: pt,
            completion_tokens: ct,
            cached_tokens: cached,
            response_model: response.model.clone(),
        },
    );
}

#[cfg(not(feature = "telemetry"))]
fn record_otel_usage(_response: &Response, _model: &str, _messages: &[Message]) {}

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

/// LlmClient backed by openai-oxide (Responses API).
///
/// With `oxide-ws` feature: call `connect_ws()` to upgrade to WebSocket mode.
/// All subsequent calls go over persistent wss:// connection (-20-25% latency).
pub struct OxideClient {
    client: OpenAI,
    pub(crate) model: String,
    pub(crate) temperature: Option<f64>,
    pub(crate) max_tokens: Option<u32>,
    /// `text.verbosity` for Responses API ("low" | "medium" | "high").
    /// `None` = let the API default apply.
    pub(crate) verbosity: Option<String>,
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
        config.apply_headers(&mut client_config);

        Ok(Self {
            client: OpenAI::with_config(client_config),
            model: config.model.clone(),
            temperature: Some(config.temp),
            max_tokens: config.max_tokens,
            verbosity: config.verbosity.clone(),
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

    /// Build a ResponseCreateRequest from messages + optional schema + optional chaining.
    ///
    /// - `previous_response_id` is None: full history as Messages format
    /// - `previous_response_id` is Some: Items format with function_call_output
    ///   (required for chaining after tool calls via Responses API)
    /// - `schema`: optional structured output json_schema config
    pub(crate) fn build_request(
        &self,
        messages: &[Message],
        schema: Option<&Value>,
        previous_response_id: Option<&str>,
    ) -> ResponseCreateRequest {
        if previous_response_id.is_some() {
            // Items format: messages + function_call_output items.
            // HTTP API accepts Messages format (without type), but WS API requires it.
            // Using Items consistently ensures both HTTP and WS work.
            return self.build_request_items(messages, previous_response_id);
        }

        // Messages format: standard request with optional structured output
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
                    let content = if msg.images.is_empty() {
                        Value::String(msg.content.clone())
                    } else {
                        serde_json::to_value(multimodal::responses_parts(&msg.content, &msg.images))
                            .unwrap_or_else(|_| Value::String(msg.content.clone()))
                    };
                    input_items.push(ResponseInputItem {
                        role: openai_oxide::types::common::Role::User,
                        content,
                    });
                }
                Role::Assistant => {
                    // Include tool call info so structured_call context shows
                    // what action was taken.
                    let mut content = msg.content.clone();
                    if !msg.tool_calls.is_empty() {
                        for tc in &msg.tool_calls {
                            let args = tc.arguments.to_string();
                            let preview = if args.len() > 200 {
                                use crate::str_ext::StrExt;
                                args.trunc(200)
                            } else {
                                &args
                            };
                            content.push_str(&format!("\n→ {}({})", tc.name, preview));
                        }
                    }
                    input_items.push(ResponseInputItem {
                        role: openai_oxide::types::common::Role::Assistant,
                        content: Value::String(content),
                    });
                }
                Role::Tool => {
                    // Clean format — no "[Tool result for ...]" prefix.
                    // The assistant message above already has the action name.
                    input_items.push(ResponseInputItem {
                        role: openai_oxide::types::common::Role::User,
                        content: Value::String(msg.content.clone()),
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
        if let Some(temp) = self.temperature
            && (temp - 1.0).abs() > f64::EPSILON
        {
            req = req.temperature(temp);
        }

        // Max tokens
        if let Some(max) = self.max_tokens {
            req = req.max_output_tokens(max as i64);
        }

        // Structured output via json_schema (and/or verbosity passthrough)
        match (schema, self.verbosity.clone()) {
            (Some(schema_val), v) => {
                req = req.text(ResponseTextConfig {
                    format: Some(ResponseTextFormat::JsonSchema {
                        name: "sgr_response".into(),
                        description: None,
                        schema: Some(schema_val.clone()),
                        strict: Some(true),
                    }),
                    verbosity: v,
                });
            }
            (None, Some(v)) => {
                req = req.text(ResponseTextConfig {
                    format: None,
                    verbosity: Some(v),
                });
            }
            (None, None) => {}
        }

        req
    }

    /// Build Items-format request for stateful chaining with previous_response_id.
    fn build_request_items(
        &self,
        messages: &[Message],
        previous_response_id: Option<&str>,
    ) -> ResponseCreateRequest {
        use openai_oxide::types::responses::ResponseInput;

        let mut items: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::Tool => {
                    if let Some(ref call_id) = msg.tool_call_id {
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
                    let content = if msg.images.is_empty() {
                        serde_json::json!(msg.content)
                    } else {
                        serde_json::to_value(multimodal::responses_parts(&msg.content, &msg.images))
                            .unwrap_or_else(|_| serde_json::json!(msg.content))
                    };
                    items.push(serde_json::json!({
                        "type": "message",
                        "role": "user",
                        "content": content,
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

        // Temperature
        if let Some(temp) = self.temperature
            && (temp - 1.0).abs() > f64::EPSILON
        {
            req = req.temperature(temp);
        }
        if let Some(max) = self.max_tokens {
            req = req.max_output_tokens(max as i64);
        }

        if let Some(v) = self.verbosity.clone() {
            req = req.text(ResponseTextConfig {
                format: None,
                verbosity: Some(v),
            });
        }

        if let Some(prev_id) = previous_response_id {
            req = req.previous_response_id(prev_id);
        }

        req
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
    ///
    /// This method does NOT use the Mutex — all state is explicit via parameters/return.
    async fn tools_call_stateful_impl(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        let mut req = self.build_request(messages, None, previous_response_id);
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
        // No Mutex save — caller owns the response_id
        record_otel_usage(&response, &self.model, messages);

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

        let chained = previous_response_id.is_some();
        let cache_pct = if input_tokens > 0 {
            (cached_tokens * 100) / input_tokens
        } else {
            0
        };

        tracing::info!(
            model = %response.model,
            response_id = %response_id,
            input_tokens,
            cached_tokens,
            cache_pct,
            chained,
            "oxide.tools_call_stateful"
        );

        if cached_tokens > 0 {
            eprintln!(
                "    💰 {}in/{}out (cached: {}, {}%)",
                input_tokens,
                response
                    .usage
                    .as_ref()
                    .and_then(|u| u.output_tokens)
                    .unwrap_or(0),
                cached_tokens,
                cache_pct
            );
        } else {
            eprintln!(
                "    💰 {}in/{}out",
                input_tokens,
                response
                    .usage
                    .as_ref()
                    .and_then(|u| u.output_tokens)
                    .unwrap_or(0)
            );
        }

        Self::check_truncation(&response)?;
        Ok((Self::extract_tool_calls(&response), Some(response_id)))
    }

    /// Check if response was truncated due to max_output_tokens.
    /// Returns Err(MaxOutputTokens) if truncated, Ok(()) otherwise.
    fn check_truncation(response: &Response) -> Result<(), SgrError> {
        let is_incomplete = response
            .status
            .as_deref()
            .is_some_and(|s| s == "incomplete");
        let is_max_tokens = response
            .incomplete_details
            .as_ref()
            .and_then(|d| d.reason.as_deref())
            .is_some_and(|r| r == "max_output_tokens");

        if is_incomplete && is_max_tokens {
            return Err(SgrError::MaxOutputTokens {
                partial_content: response.output_text(),
            });
        }
        Ok(())
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
        // Make schema OpenAI-strict — UNLESS it's already strict
        // (build_action_schema produces pre-strict schemas that ensure_strict would break)
        let strict_schema =
            if schema.get("additionalProperties").and_then(|v| v.as_bool()) == Some(false) {
                // Already strict-compatible (e.g., from build_action_schema)
                schema.clone()
            } else {
                let mut s = schema.clone();
                openai_oxide::parsing::ensure_strict(&mut s);
                s
            };

        // Stateless — build request with full message history, no chaining.
        // store(true) enables server-side prompt caching for stable prefix.
        let mut req = self.build_request(messages, Some(&strict_schema), None);
        req = req.store(true);

        let span = tracing::info_span!(
            "oxide.responses.create",
            model = %self.model,
            method = "structured_call",
        );
        let _enter = span.enter();

        // Debug: dump schema on first call
        if std::env::var("SGR_DEBUG_SCHEMA").is_ok()
            && let Some(ref text_cfg) = req.text
        {
            eprintln!(
                "[sgr] Schema: {}",
                serde_json::to_string(text_cfg).unwrap_or_default()
            );
        }

        let response = self.send_request_auto(req).await?;

        // No Mutex save — structured_call is stateless
        record_otel_usage(&response, &self.model, messages);

        Self::check_truncation(&response)?;

        let raw_text = response.output_text();
        if std::env::var("SGR_DEBUG").is_ok() {
            eprintln!("[sgr] Raw response: {}", {
                use crate::str_ext::StrExt;
                raw_text.trunc(500)
            });
        }
        let tool_calls = Self::extract_tool_calls(&response);
        let parsed = serde_json::from_str::<Value>(&raw_text).ok();

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
        let cache_pct = if input_tokens > 0 {
            (cached_tokens * 100) / input_tokens
        } else {
            0
        };

        {
            let output_tokens = response
                .usage
                .as_ref()
                .and_then(|u| u.output_tokens)
                .unwrap_or(0);
            if cached_tokens > 0 {
                eprintln!(
                    "    💰 {}in/{}out (cached: {}, {}%)",
                    input_tokens, output_tokens, cached_tokens, cache_pct
                );
            } else {
                eprintln!("    💰 {}in/{}out", input_tokens, output_tokens);
            }
        }

        Ok((parsed, tool_calls, raw_text))
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        // Stateless — no previous_response_id, full message history.
        // store(true) enables server-side prompt caching: OpenAI auto-caches
        // the stable prefix (system prompt + tools) for requests >1024 tokens.
        let mut req = self.build_request(messages, None, None);
        req = req.store(true);

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
                strict: None, // AI-NOTE: strict=true breaks tools with optional params
            })
            .collect();
        req = req.tools(response_tools);

        // Force model to always call a tool — prevents text-only responses
        // that lose answer content (tools_call only returns Vec<ToolCall>).
        req = req.tool_choice(openai_oxide::types::responses::ResponseToolChoice::Mode(
            "required".into(),
        ));
        // AI-NOTE: explicit parallel_tool_calls=true for OpenAI. Anthropic models on OpenRouter
        // reject this param (404); they use `disable_parallel_tool_use` natively, not exposed here.
        if !self.model.contains("anthropic/") && !self.model.starts_with("claude") {
            req = req.parallel_tool_calls(true);
        }

        let response = self.send_request_auto(req).await?;

        record_otel_usage(&response, &self.model, messages);
        Self::check_truncation(&response)?;

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
        let cache_pct = if input_tokens > 0 {
            (cached_tokens * 100) / input_tokens
        } else {
            0
        };

        if cached_tokens > 0 {
            eprintln!(
                "    💰 {}in/{}out (cached: {}, {}%)",
                input_tokens,
                response
                    .usage
                    .as_ref()
                    .and_then(|u| u.output_tokens)
                    .unwrap_or(0),
                cached_tokens,
                cache_pct
            );
        } else {
            eprintln!(
                "    💰 {}in/{}out",
                input_tokens,
                response
                    .usage
                    .as_ref()
                    .and_then(|u| u.output_tokens)
                    .unwrap_or(0)
            );
        }

        let calls = Self::extract_tool_calls(&response);
        Ok(calls)
    }

    async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        self.tools_call_stateful_impl(messages, tools, previous_response_id)
            .await
    }

    /// tool_choice=auto so model can emit reasoning text ALONGSIDE tool calls in one response.
    /// Returns (tool_calls, reasoning_text). Used by single-phase agent to get 1 LLM call/step.
    async fn tools_call_with_text(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<(Vec<ToolCall>, String), SgrError> {
        let mut req = self.build_request(messages, None, None);
        req = req.store(true);

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
        // AI-NOTE: tool_choice=auto (not required) — model can return text+tools in same response.
        // This is the key for single-phase: reasoning in text, action in tool calls, 1 LLM call.
        req = req.tool_choice(openai_oxide::types::responses::ResponseToolChoice::Mode(
            "auto".into(),
        ));
        req = req.parallel_tool_calls(true);

        let response = self.send_request_auto(req).await?;

        record_otel_usage(&response, &self.model, messages);
        Self::check_truncation(&response)?;

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
        let cache_pct = if input_tokens > 0 {
            (cached_tokens * 100) / input_tokens
        } else {
            0
        };
        let output_tokens = response
            .usage
            .as_ref()
            .and_then(|u| u.output_tokens)
            .unwrap_or(0);
        if cached_tokens > 0 {
            eprintln!(
                "    💰 {}in/{}out (cached: {}, {}%)",
                input_tokens, output_tokens, cached_tokens, cache_pct
            );
        } else {
            eprintln!("    💰 {}in/{}out", input_tokens, output_tokens);
        }

        let text = response.output_text();
        let calls = Self::extract_tool_calls(&response);
        Ok((calls, text))
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        let mut req = self.build_request(messages, None, None);
        req = req.store(true);

        let response = self.send_request_auto(req).await?;

        record_otel_usage(&response, &self.model, messages);
        Self::check_truncation(&response)?;

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
    use crate::types::ImagePart;

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
        let req = client.build_request(&messages, None, None);
        assert_eq!(req.model, "gpt-5.4");
        assert!(req.instructions.is_none());
        assert!(req.input.is_some());
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
        let req = client.build_request(&[Message::user("Hi")], Some(&schema), None);
        assert!(req.text.is_some());
    }

    #[test]
    fn build_request_stateless_no_previous_response_id() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = OxideClient::from_config(&config).unwrap();

        let req = client.build_request(&[Message::user("Hi")], None, None);
        assert!(
            req.previous_response_id.is_none(),
            "build_request must be stateless when no explicit ID"
        );
    }

    #[test]
    fn build_request_explicit_chaining() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = OxideClient::from_config(&config).unwrap();

        // With previous_response_id — uses Items format for chaining
        let req = client.build_request(&[Message::user("Hi")], None, Some("resp_xyz"));
        assert_eq!(
            req.previous_response_id.as_deref(),
            Some("resp_xyz"),
            "build_request should chain with explicit previous_response_id"
        );
    }

    #[test]
    fn build_request_tool_outputs_chaining() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = OxideClient::from_config(&config).unwrap();

        // With previous_response_id — tool outputs as function_call_output items
        let messages = vec![Message::tool("call_1", "result data")];
        let req = client.build_request(&messages, None, Some("resp_123"));
        assert_eq!(req.previous_response_id.as_deref(), Some("resp_123"));

        // Without previous_response_id
        let req = client.build_request(&messages, None, None);
        assert!(
            req.previous_response_id.is_none(),
            "build_request must be stateless when no explicit ID"
        );
    }

    #[test]
    fn build_request_multimodal_user() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = OxideClient::from_config(&config).unwrap();
        let img = ImagePart {
            data: "AAAA".into(),
            mime_type: "image/jpeg".into(),
        };
        let messages = vec![Message::user_with_images("Describe this", vec![img])];
        let req = client.build_request(&messages, None, None);

        // Single user with images must serialize as content-parts array, not string
        let input = req.input.as_ref().expect("input missing");
        let serialized = serde_json::to_value(input).unwrap();
        let s = serde_json::to_string(&serialized).unwrap();
        assert!(s.contains("input_text"), "missing input_text part: {s}");
        assert!(s.contains("input_image"), "missing input_image part: {s}");
        assert!(
            s.contains("data:image/jpeg;base64,AAAA"),
            "missing data URL: {s}"
        );
    }

    #[test]
    fn build_request_items_multimodal_user() {
        let config = LlmConfig::with_key("sk-test", "gpt-5.4");
        let client = OxideClient::from_config(&config).unwrap();
        let img = ImagePart {
            data: "BBBB".into(),
            mime_type: "image/png".into(),
        };
        let messages = vec![Message::user_with_images("What's on screen?", vec![img])];
        // previous_response_id triggers items-format path
        let req = client.build_request(&messages, None, Some("resp_prev"));

        let input = req.input.as_ref().expect("input missing");
        let s = serde_json::to_string(input).unwrap();
        assert!(
            s.contains("input_text"),
            "items path missing input_text: {s}"
        );
        assert!(
            s.contains("input_image"),
            "items path missing input_image: {s}"
        );
        assert!(
            s.contains("data:image/png;base64,BBBB"),
            "items path missing data URL: {s}"
        );
    }
}
