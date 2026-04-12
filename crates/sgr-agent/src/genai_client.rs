//! GenaiClient — internal LlmClient adapter for the `genai` crate.
//!
//! This is an implementation detail. Use `Llm` + `LlmConfig` as the public API.

use crate::client::LlmClient;
use crate::tool::ToolDef;
use crate::types::{Message, Role, SgrError, ToolCall};
use futures::StreamExt;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest, ChatResponse, ChatResponseFormat, ChatStreamEvent,
    ContentPart, JsonSpec, MessageContent, Tool, ToolResponse,
};
use serde_json::Value;
use tracing::Instrument;

/// LlmClient adapter wrapping genai's multi-provider Client.
/// Use `Llm` as the public facade — this type is an implementation detail.
pub struct GenaiClient {
    client: genai::Client,
    pub(crate) model: String,
    pub(crate) temperature: Option<f64>,
    pub(crate) max_tokens: Option<u32>,
    /// OpenAI prompt cache key — caches the system prompt prefix server-side.
    pub(crate) prompt_cache_key: Option<String>,
}

impl GenaiClient {
    /// Create from a pre-configured genai Client and model name.
    pub fn new(client: genai::Client, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            temperature: None,
            max_tokens: None,
            prompt_cache_key: None,
        }
    }

    /// Create from LlmConfig — routes by api_key × base_url matrix.
    /// Vertex AI: when `project_id` is set, uses gcloud ADC with AuthResolver.
    pub(crate) fn from_config(config: &crate::types::LlmConfig) -> Self {
        // Vertex AI — needs per-request auth via gcloud
        if let Some(ref project_id) = config.project_id {
            let location = config.location.as_deref().unwrap_or("global").to_string();
            let mut client = Self::vertex_ai(project_id, &location, &config.model);
            client.temperature = Some(config.temp);
            client.max_tokens = config.max_tokens;
            client.prompt_cache_key = config.prompt_cache_key.clone();
            return client;
        }

        let mut client = match (&config.api_key, &config.base_url) {
            (Some(key), Some(url)) => Self::custom_endpoint(key, url, &config.model),
            (Some(key), None) => Self::with_api_key(key, &config.model),
            (None, Some(url)) => {
                tracing::warn!("No API key for custom endpoint {url} — auth may fail");
                Self::custom_endpoint("", url, &config.model)
            }
            (None, None) => Self::from_model(&config.model),
        };
        client.temperature = Some(config.temp);
        client.max_tokens = config.max_tokens;
        client.prompt_cache_key = config.prompt_cache_key.clone();
        client
    }

    /// Create for Vertex AI using gcloud ADC (Application Default Credentials).
    /// Calls `gcloud auth print-access-token` per request for auth.
    fn vertex_ai(project_id: &str, location: &str, model: impl Into<String>) -> Self {
        use genai::resolver::{AuthData, AuthResolver};
        use genai::{Headers, ModelIden};
        use std::pin::Pin;
        use std::sync::Arc;

        let project_id: Arc<str> = project_id.into();
        let location: Arc<str> = location.into();

        let resolve_fn = move |model: ModelIden| -> Pin<
            Box<
                dyn std::future::Future<Output = Result<Option<AuthData>, genai::resolver::Error>>
                    + Send
                    + 'static,
            >,
        > {
            let project_id = project_id.clone();
            let location = location.clone();
            Box::pin(async move {
                let output = tokio::process::Command::new("gcloud")
                    .args(["auth", "print-access-token"])
                    .output()
                    .await
                    .map_err(|e| genai::resolver::Error::Custom(format!("gcloud error: {e}")))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(genai::resolver::Error::Custom(format!(
                        "gcloud auth failed: {stderr}"
                    )));
                }

                let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let url = format!(
                    "https://{location}-aiplatform.googleapis.com/v1/projects/{project_id}/locations/{location}/publishers/google/models/{}:generateContent",
                    model.model_name
                );

                let auth_value = format!("Bearer {token}");
                let auth_header = Headers::from(("Authorization", auth_value));
                Ok(Some(AuthData::RequestOverride {
                    headers: auth_header,
                    url,
                }))
            })
        };

        let auth_resolver = AuthResolver::from_resolver_async_fn(resolve_fn);
        let client = genai::Client::builder()
            .with_auth_resolver(auth_resolver)
            .build();
        Self::new(client, model)
    }

    /// Create with default genai Client (uses env vars for auth).
    /// Model name auto-detects provider: "gpt-*" → OpenAI, "claude-*" → Anthropic, etc.
    pub fn from_model(model: impl Into<String>) -> Self {
        Self::new(genai::Client::default(), model)
    }

    /// Create with explicit API key + auto-detect provider from model name.
    fn with_api_key(api_key: &str, model: impl Into<String>) -> Self {
        use genai::ServiceTarget;
        use genai::resolver::{AuthData, ServiceTargetResolver};

        let api_key = api_key.to_string();
        let target_resolver = ServiceTargetResolver::from_resolver_fn(
            move |service_target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                let auth = AuthData::from_single(api_key.clone());
                Ok(ServiceTarget {
                    auth,
                    ..service_target
                })
            },
        );

        let client = genai::Client::builder()
            .with_service_target_resolver(target_resolver)
            .build();
        Self::new(client, model)
    }

    /// Create for any OpenAI-compatible endpoint (OpenRouter, Ollama, LiteLLM, etc.).
    /// `base_url` should be the API base (e.g. `https://openrouter.ai/api/v1`),
    /// NOT including `/chat/completions` — genai appends that automatically.
    ///
    /// Adapter selection:
    /// - Default: `OpenAI` (Chat Completions) — correct for most proxies
    /// - Explicit: use namespace prefix to override, e.g. `openai_resp::gpt-5.4`
    ///   routes through the Responses API adapter (`/responses` endpoint)
    pub fn custom_endpoint(api_key: &str, base_url: &str, model: impl Into<String>) -> Self {
        use genai::adapter::AdapterKind;
        use genai::resolver::{AuthData, Endpoint, ServiceTargetResolver};
        use genai::{ModelIden, ServiceTarget};

        let model_str: String = model.into();
        // If the model has an explicit namespace (e.g. "openai_resp::gpt-5.4"),
        // respect the user's adapter choice. Otherwise default to OpenAI (Chat Completions).
        let explicit_adapter = model_str.contains("::");

        let api_key = api_key.to_string();
        // Strip /chat/completions if caller included it — genai adds it automatically
        let mut url = base_url
            .trim_end_matches('/')
            .trim_end_matches("/chat/completions")
            .to_string();
        // Ensure trailing slash for URL join to work correctly
        url.push('/');
        let target_resolver = ServiceTargetResolver::from_resolver_fn(
            move |service_target: ServiceTarget| -> Result<ServiceTarget, genai::resolver::Error> {
                let ServiceTarget { model, .. } = service_target;
                let endpoint = Endpoint::from_owned(url.clone());
                let auth = AuthData::from_single(api_key.clone());
                let adapter = if explicit_adapter {
                    model.adapter_kind // User explicitly chose via namespace
                } else {
                    AdapterKind::OpenAI // Default for custom endpoints
                };
                let model = ModelIden::new(adapter, model.model_name);
                Ok(ServiceTarget {
                    endpoint,
                    auth,
                    model,
                })
            },
        );

        let client = genai::Client::builder()
            .with_service_target_resolver(target_resolver)
            .build();
        Self::new(client, model_str)
    }

    /// Build a ChatRequest from our Message slice.
    fn build_request(&self, messages: &[Message]) -> ChatRequest {
        let mut chat_msgs = Vec::new();
        let mut system_text: Option<String> = None;

        let mut i = 0;
        while i < messages.len() {
            let msg = &messages[i];
            match msg.role {
                Role::System => {
                    match &mut system_text {
                        Some(text) => {
                            text.push_str("\n\n");
                            text.push_str(&msg.content);
                        }
                        None => system_text = Some(msg.content.clone()),
                    }
                    i += 1;
                }
                Role::User => {
                    chat_msgs.push(ChatMessage::user(&msg.content));
                    i += 1;
                }
                Role::Assistant => {
                    if !msg.tool_calls.is_empty() {
                        let mut parts = Vec::new();
                        if !msg.content.is_empty() {
                            parts.push(ContentPart::Text(msg.content.clone()));
                        }
                        for tc in &msg.tool_calls {
                            parts.push(ContentPart::ToolCall(genai::chat::ToolCall {
                                call_id: tc.id.clone(),
                                fn_name: tc.name.clone(),
                                fn_arguments: tc.arguments.clone(),
                                thought_signatures: None,
                            }));
                        }
                        chat_msgs.push(ChatMessage::assistant(MessageContent::from_parts(parts)));
                        i += 1;

                        // Collect consecutive Tool responses
                        while i < messages.len() && messages[i].role == Role::Tool {
                            let tool_msg = &messages[i];
                            let call_id = tool_msg
                                .tool_call_id
                                .as_deref()
                                .unwrap_or("unknown")
                                .to_string();
                            chat_msgs.push(ChatMessage::from(ToolResponse {
                                call_id,
                                content: tool_msg.content.clone().into(),
                            }));
                            i += 1;
                        }
                    } else {
                        chat_msgs.push(ChatMessage::assistant(&msg.content));
                        i += 1;
                    }
                }
                Role::Tool => {
                    // Orphaned tool response (no preceding assistant with tool_calls)
                    while i < messages.len() && messages[i].role == Role::Tool {
                        let tool_msg = &messages[i];
                        let call_id = tool_msg
                            .tool_call_id
                            .as_deref()
                            .unwrap_or("unknown")
                            .to_string();
                        chat_msgs.push(ChatMessage::from(ToolResponse {
                            call_id,
                            content: tool_msg.content.clone().into(),
                        }));
                        i += 1;
                    }
                }
            }
        }

        let mut req = ChatRequest::from_messages(chat_msgs);
        if let Some(sys) = system_text {
            req = req.with_system(&sys);
        }
        req
    }

    /// Build chat options from our config.
    fn build_options(&self) -> Option<ChatOptions> {
        if self.temperature.is_none()
            && self.max_tokens.is_none()
            && self.prompt_cache_key.is_none()
        {
            return None;
        }
        let mut opts = ChatOptions::default();
        if let Some(temp) = self.temperature {
            opts = opts.with_temperature(temp);
        }
        if let Some(max) = self.max_tokens {
            opts = opts.with_max_tokens(max);
        }
        if let Some(ref key) = self.prompt_cache_key {
            opts = opts.with_prompt_cache_key(key);
        }
        Some(opts)
    }

    /// Execute chat and return response.
    /// Instrumented with GenAI span conventions for OTEL export (LangSmith, etc.).
    ///
    /// Uses native OpenTelemetry SDK for spans (not tracing::info_span!) because
    /// tracing-opentelemetry doesn't export set_attribute() calls made inside
    /// .instrument() blocks. Native OTEL spans support set_attribute at any time.
    async fn exec(&self, req: ChatRequest) -> Result<ChatResponse, SgrError> {
        // Use native OTEL tracer for the span (not tracing crate).
        // tracing-opentelemetry doesn't export set_attribute() inside .instrument(),
        // so we use the OTEL SDK directly — like the Python LangSmith examples.
        #[cfg(feature = "telemetry")]
        let otel_cx = {
            use opentelemetry::trace::{Span, TraceContextExt, Tracer, TracerProvider};
            let provider = opentelemetry::global::tracer_provider();
            let tracer = provider.tracer("sgr-agent");
            let mut otel_span = tracer.start("gen_ai.chat");

            // AI-NOTE: session.id — Phoenix Sessions tab groups spans by this value
            if let Some(sid) = crate::telemetry::session_id() {
                otel_span.set_attribute(opentelemetry::KeyValue::new("session.id", sid));
            }

            // LangSmith conventions
            otel_span.set_attribute(opentelemetry::KeyValue::new("langsmith.span.kind", "LLM"));
            otel_span.set_attribute(opentelemetry::KeyValue::new("gen_ai.system", "OpenRouter"));
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "gen_ai.request.model",
                self.model.clone(),
            ));
            otel_span.set_attribute(opentelemetry::KeyValue::new("llm.request.type", "chat"));
            // Phoenix / OpenInference conventions
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "openinference.span.kind",
                "LLM",
            ));
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "llm.model_name",
                strip_model_namespace(&self.model).to_string(),
            ));

            // Input messages: serialize as gen_ai.prompt.{i}.role/content
            // Use serde since ChatMessage variants are complex
            for (i, msg) in req.messages.iter().enumerate() {
                let json = serde_json::to_string(msg).unwrap_or_default();
                // Extract role from JSON
                let role = if json.contains("\"role\":\"user\"") {
                    "user"
                } else if json.contains("\"role\":\"assistant\"") {
                    "assistant"
                } else {
                    "system"
                };
                // Content: truncate to 4KB for OTEL attr limit
                let content = if json.len() > 4000 {
                    format!("{}...", &json[..4000])
                } else {
                    json
                };
                otel_span.set_attribute(opentelemetry::KeyValue::new(
                    format!("gen_ai.prompt.{i}.role"),
                    role.to_string(),
                ));
                otel_span.set_attribute(opentelemetry::KeyValue::new(
                    format!("gen_ai.prompt.{i}.content"),
                    content,
                ));
            }
            // Also system prompt if present
            if let Some(ref sys) = req.system {
                otel_span.set_attribute(opentelemetry::KeyValue::new(
                    "gen_ai.prompt.system",
                    if sys.len() > 4000 {
                        format!("{}...", &sys[..4000])
                    } else {
                        sys.clone()
                    },
                ));
            }

            // AI-NOTE: input.value + mime_type — Phoenix renders formatted JSON in detail view
            let input_json = req.messages.iter().rev().find_map(|m| {
                let json = serde_json::to_string(m).unwrap_or_default();
                if json.contains("\"role\":\"user\"") || json.contains("\"role\":\"tool\"") {
                    let content = serde_json::from_str::<serde_json::Value>(&json)
                        .ok()
                        .and_then(|v| v.get("content")?.as_str().map(String::from))
                        .unwrap_or(json);
                    let content = if content.len() > 2000 {
                        format!("{}...", &content[..2000])
                    } else {
                        content
                    };
                    Some(serde_json::json!({"role": "user", "content": content}))
                } else {
                    None
                }
            });
            if let Some(input) = &input_json {
                otel_span.set_attribute(opentelemetry::KeyValue::new(
                    "input.value",
                    input.to_string(),
                ));
                otel_span.set_attribute(opentelemetry::KeyValue::new(
                    "input.mime_type",
                    "application/json",
                ));
            }

            opentelemetry::Context::current().with_span(otel_span)
        };

        let response = self
            .client
            .exec_chat(&self.model, req, self.build_options().as_ref())
            .await
            .map_err(map_genai_error)?;

        // Record token usage + output on the OTEL span
        #[cfg(feature = "telemetry")]
        {
            use opentelemetry::trace::{Span, TraceContextExt};
            let otel_span = otel_cx.span();
            let pt = response.usage.prompt_tokens.unwrap_or(0);
            let ct = response.usage.completion_tokens.unwrap_or(0);

            // Output (completion)
            let output_text = response.first_text().unwrap_or("").to_string();
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "gen_ai.completion.0.role",
                "assistant",
            ));
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "gen_ai.completion.0.content",
                if output_text.len() > 4000 {
                    format!("{}...", &output_text[..4000])
                } else {
                    output_text
                },
            ));

            // AI-NOTE: output.value + mime_type — Phoenix renders formatted JSON
            let out_text = response.first_text().unwrap_or("").to_string();
            let tcs = response.tool_calls();
            let output_json = if !out_text.is_empty() {
                let text = if out_text.len() > 2000 {
                    format!("{}...", &out_text[..2000])
                } else {
                    out_text
                };
                Some(serde_json::json!({"role": "assistant", "content": text}))
            } else if !tcs.is_empty() {
                let calls: Vec<serde_json::Value> = tcs
                    .into_iter()
                    .map(|tc| serde_json::json!({"name": tc.fn_name, "arguments": tc.fn_arguments}))
                    .collect();
                Some(serde_json::json!({"role": "assistant", "tool_calls": calls}))
            } else {
                None
            };
            if let Some(output) = &output_json {
                otel_span.set_attribute(opentelemetry::KeyValue::new(
                    "output.value",
                    output.to_string(),
                ));
                otel_span.set_attribute(opentelemetry::KeyValue::new(
                    "output.mime_type",
                    "application/json",
                ));
            }

            // Token usage — GenAI conventions (LangSmith)
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "gen_ai.usage.prompt_tokens",
                i64::from(pt),
            ));
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "gen_ai.usage.completion_tokens",
                i64::from(ct),
            ));
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "gen_ai.usage.total_tokens",
                i64::from(pt + ct),
            ));
            otel_span.set_attribute(opentelemetry::KeyValue::new(
                "gen_ai.response.model",
                response.provider_model_iden.model_name.to_string(),
            ));
            // Token usage — OpenInference conventions (Phoenix)
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

            // End span (sets end_time, triggers export)
            otel_span.end();
        }

        // Also log for file telemetry (tracing)
        tracing::info!(
            model = %self.model,
            prompt_tokens = response.usage.prompt_tokens.unwrap_or(0),
            completion_tokens = response.usage.completion_tokens.unwrap_or(0),
            "gen_ai.chat"
        );

        Ok(response)
    }

    /// Extract tool calls from a ChatResponse.
    fn extract_tool_calls(response: &ChatResponse) -> Vec<ToolCall> {
        response
            .tool_calls()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.call_id.clone(),
                name: tc.fn_name.clone(),
                arguments: tc.fn_arguments.clone(),
            })
            .collect()
    }

    /// Extract text from a ChatResponse.
    fn extract_text(response: &ChatResponse) -> String {
        response.first_text().unwrap_or("").to_string()
    }

    /// Stream text completion, calling `on_token` for each text chunk.
    /// Returns the full concatenated text.
    pub async fn stream_complete<F>(
        &self,
        messages: &[Message],
        mut on_token: F,
    ) -> Result<String, SgrError>
    where
        F: FnMut(&str),
    {
        let span = tracing::info_span!(
            "gen_ai.stream",
            model = %self.model,
        );

        #[cfg(feature = "telemetry")]
        {
            use tracing_opentelemetry::OpenTelemetrySpanExt;
            span.set_attribute("langsmith.span.kind", "llm");
            span.set_attribute("gen_ai.operation.name", "chat");
            span.set_attribute("gen_ai.request.model", self.model.clone());
            // Phoenix / OpenInference conventions
            span.set_attribute("openinference.span.kind", "LLM");
            span.set_attribute(
                "llm.model_name",
                strip_model_namespace(&self.model).to_string(),
            );
        }

        async {
            let req = self.build_request(messages);
            let opts = self.build_options();
            let stream_resp = self
                .client
                .exec_chat_stream(&self.model, req, opts.as_ref())
                .await
                .map_err(map_genai_error)?;

            let mut stream = stream_resp.stream;
            let mut full_text = String::new();

            while let Some(event) = stream.next().await {
                match event.map_err(map_genai_error)? {
                    ChatStreamEvent::Chunk(chunk) => {
                        full_text.push_str(&chunk.content);
                        on_token(&chunk.content);
                    }
                    ChatStreamEvent::End(_) => break,
                    _ => {}
                }
            }

            if full_text.is_empty() {
                return Err(SgrError::EmptyResponse);
            }
            Ok(full_text)
        }
        .instrument(span)
        .await
    }

    /// Function calling with stateful session support (OpenAI Responses API).
    /// When `previous_response_id` is set, the server uses cached conversation state.
    /// Always sets `store: true` so response_id can be used for future stateful calls.
    pub async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        let mut req = self.build_request(messages);
        let genai_tools: Vec<Tool> = tools.iter().map(to_genai_tool).collect();
        req = req.with_tools(genai_tools);
        // Always store for stateful sessions — enables response_id reuse
        req.store = Some(true);
        if let Some(prev_id) = previous_response_id {
            req.previous_response_id = Some(prev_id.to_string());
        }

        let response = self.exec(req).await?;
        let tool_calls = Self::extract_tool_calls(&response);
        let response_id = response.response_id;
        Ok((tool_calls, response_id))
    }
}

/// Convert ToolDef to genai Tool.
fn to_genai_tool(def: &ToolDef) -> Tool {
    let mut tool = Tool::new(&def.name);
    if !def.description.is_empty() {
        tool = tool.with_description(&def.description);
    }
    tool = tool.with_schema(def.parameters.clone());
    tool
}

/// Strip provider namespace from model name for Phoenix/OpenInference pricing.
/// e.g. "openai/gpt-5.4" → "gpt-5.4", "anthropic/claude-sonnet-4.6" → "claude-sonnet-4.6"
fn strip_model_namespace(model: &str) -> &str {
    model.rsplit_once('/').map_or(model, |(_, name)| name)
}

/// Map genai error to our SgrError.
fn map_genai_error(e: genai::Error) -> SgrError {
    let msg = e.to_string();
    if msg.contains("429") || msg.contains("rate") || msg.contains("quota") {
        SgrError::Api {
            status: 429,
            body: msg,
        }
    } else if msg.contains("400") || msg.contains("INVALID") {
        SgrError::Api {
            status: 400,
            body: msg,
        }
    } else if msg.contains("401") || msg.contains("auth") || msg.contains("key") {
        SgrError::Api {
            status: 401,
            body: msg,
        }
    } else {
        SgrError::Schema(msg)
    }
}

#[async_trait::async_trait]
impl LlmClient for GenaiClient {
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
        let req = self.build_request(messages);

        // OpenAI strict mode: additionalProperties:false + all properties required
        let mut strict_schema = schema.clone();
        crate::schema::make_openai_strict(&mut strict_schema);
        let json_spec = JsonSpec::new("sgr_response", strict_schema);
        let mut opts = self.build_options().unwrap_or_default();
        opts = opts.with_response_format(ChatResponseFormat::JsonSpec(json_spec));

        let response = self
            .client
            .exec_chat(&self.model, req, Some(&opts))
            .await
            .map_err(map_genai_error)?;

        let raw_text = Self::extract_text(&response);
        let tool_calls = Self::extract_tool_calls(&response);
        let parsed = serde_json::from_str::<Value>(&raw_text).ok();

        Ok((parsed, tool_calls, raw_text))
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        let mut req = self.build_request(messages);
        let genai_tools: Vec<Tool> = tools.iter().map(to_genai_tool).collect();
        req = req.with_tools(genai_tools);

        let response = self.exec(req).await?;
        Ok(Self::extract_tool_calls(&response))
    }

    async fn tools_call_stateful(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
        previous_response_id: Option<&str>,
    ) -> Result<(Vec<ToolCall>, Option<String>), SgrError> {
        // Delegate to the inherent method
        GenaiClient::tools_call_stateful(self, messages, tools, previous_response_id).await
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        let req = self.build_request(messages);
        let response = self.exec(req).await?;
        let text = Self::extract_text(&response);
        if text.is_empty() {
            return Err(SgrError::EmptyResponse);
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genai::chat::ChatRole;

    #[test]
    fn to_genai_tool_maps_correctly() {
        let def = ToolDef {
            name: "read_file".to_string(),
            description: "Read a file".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"}
                },
                "required": ["path"]
            }),
        };
        let tool = to_genai_tool(&def);
        assert_eq!(tool.name, "read_file".into());
        assert_eq!(tool.description.as_deref(), Some("Read a file"));
        assert!(tool.schema.is_some());
    }

    #[test]
    fn build_request_basic() {
        let client = GenaiClient::from_model("test-model");
        let messages = vec![
            Message::system("You are helpful."),
            Message::user("Hello"),
            Message::assistant("Hi!"),
        ];
        let req = client.build_request(&messages);
        assert_eq!(req.system.as_deref(), Some("You are helpful."));
        assert_eq!(req.messages.len(), 2); // user + assistant
    }

    #[test]
    fn build_request_with_tool_calls() {
        let client = GenaiClient::from_model("test-model");
        let messages = vec![
            Message::user("read file"),
            Message::assistant_with_tool_calls(
                "Reading...",
                vec![ToolCall {
                    id: "call1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "test.rs"}),
                }],
            ),
            Message::tool("call1", "file contents here"),
        ];
        let req = client.build_request(&messages);
        // user + assistant(with tool calls) + tool response
        assert_eq!(req.messages.len(), 3);
    }

    #[test]
    fn build_request_tool_responses_have_tool_role() {
        let client = GenaiClient::from_model("test-model");
        let messages = vec![
            Message::user("do it"),
            Message::assistant_with_tool_calls(
                "",
                vec![
                    ToolCall {
                        id: "c1".into(),
                        name: "a".into(),
                        arguments: serde_json::json!({}),
                    },
                    ToolCall {
                        id: "c2".into(),
                        name: "b".into(),
                        arguments: serde_json::json!({}),
                    },
                ],
            ),
            Message::tool("c1", "result1"),
            Message::tool("c2", "result2"),
        ];
        let req = client.build_request(&messages);
        // user + assistant + tool_response1 + tool_response2
        assert_eq!(req.messages.len(), 4);
        assert_eq!(req.messages[2].role, ChatRole::Tool);
        assert_eq!(req.messages[3].role, ChatRole::Tool);
    }

    #[test]
    fn build_request_multiple_systems_merged() {
        let client = GenaiClient::from_model("test-model");
        let messages = vec![
            Message::system("Part 1"),
            Message::system("Part 2"),
            Message::user("Go"),
        ];
        let req = client.build_request(&messages);
        let sys = req.system.unwrap();
        assert!(sys.contains("Part 1"));
        assert!(sys.contains("Part 2"));
    }

    #[test]
    fn genai_client_from_model() {
        let client = GenaiClient::from_model("gpt-4o-mini");
        assert_eq!(client.model, "gpt-4o-mini");
    }

    #[test]
    fn genai_client_from_config_options() {
        let config =
            crate::types::LlmConfig::endpoint("sk-test", "https://api.example.com/v1", "my-model")
                .temperature(0.7)
                .max_tokens(1000);
        let client = GenaiClient::from_config(&config);
        assert_eq!(client.temperature, Some(0.7));
        assert_eq!(client.max_tokens, Some(1000));
    }
}
