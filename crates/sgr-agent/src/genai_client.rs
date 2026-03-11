//! GenaiClient — LlmClient adapter for the `genai` crate.
//!
//! Wraps genai's unified multi-provider API as our LlmClient trait.
//! Supports 14+ providers: OpenAI, Anthropic, Gemini, Cohere, xAI, Ollama, etc.
//!
//! ```no_run
//! use sgr_agent::genai_client::GenaiClient;
//!
//! let client = GenaiClient::from_model("claude-3-haiku-20240307");
//! // or: GenaiClient::new(custom_genai_client, "gpt-4o-mini");
//! ```

use crate::client::LlmClient;
use crate::tool::ToolDef;
use crate::types::{Message, Role, SgrError, ToolCall};
use genai::chat::{
    ChatMessage, ChatRequest, ChatResponse, ContentPart, MessageContent, Tool, ToolResponse,
};
use serde_json::Value;

/// LlmClient adapter wrapping genai's multi-provider Client.
pub struct GenaiClient {
    client: genai::Client,
    model: String,
    temperature: Option<f64>,
    max_tokens: Option<u32>,
}

impl GenaiClient {
    /// Create from a pre-configured genai Client and model name.
    pub fn new(client: genai::Client, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            temperature: None,
            max_tokens: None,
        }
    }

    /// Create with default genai Client (uses env vars for auth).
    /// Model name auto-detects provider: "gpt-*" → OpenAI, "claude-*" → Anthropic, etc.
    pub fn from_model(model: impl Into<String>) -> Self {
        Self::new(genai::Client::default(), model)
    }

    /// Set temperature for completions.
    pub fn with_temperature(mut self, temp: f64) -> Self {
        self.temperature = Some(temp);
        self
    }

    /// Set max output tokens.
    pub fn with_max_tokens(mut self, max: u32) -> Self {
        self.max_tokens = Some(max);
        self
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
    fn build_options(&self) -> Option<genai::chat::ChatOptions> {
        if self.temperature.is_none() && self.max_tokens.is_none() {
            return None;
        }
        let mut opts = genai::chat::ChatOptions::default();
        if let Some(temp) = self.temperature {
            opts = opts.with_temperature(temp);
        }
        if let Some(max) = self.max_tokens {
            opts = opts.with_max_tokens(max);
        }
        Some(opts)
    }

    /// Execute chat and return response.
    async fn exec(&self, req: ChatRequest) -> Result<ChatResponse, SgrError> {
        self.client
            .exec_chat(&self.model, req, self.build_options().as_ref())
            .await
            .map_err(map_genai_error)
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
        let schema_hint = format!(
            "\n\nRespond with valid JSON matching this schema:\n{}\n\nDo NOT wrap in markdown code blocks. Output raw JSON only.",
            serde_json::to_string_pretty(schema).unwrap_or_default()
        );

        let mut req = self.build_request(messages);
        let current_system = req.system.take().unwrap_or_default();
        req = req.with_system(format!("{}{}", current_system, schema_hint));

        let response = self.exec(req).await?;
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
        assert_eq!(tool.name, "read_file");
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
    fn genai_client_with_options() {
        let client = GenaiClient::from_model("test")
            .with_temperature(0.7)
            .with_max_tokens(1000);
        assert_eq!(client.temperature, Some(0.7));
        assert_eq!(client.max_tokens, Some(1000));
    }
}
