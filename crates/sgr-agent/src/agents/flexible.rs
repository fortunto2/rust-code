//! FlexibleAgent — text-based agent for weak models without structured output.
//!
//! Puts tool descriptions in the system prompt, sends plain completion,
//! then uses flexible_parser + coerce to extract tool calls from text.
//!
//! Supports retry with error feedback: if parsing fails, accumulates errors
//! and feeds them back to the model in the next attempt (up to max_retries).

use crate::agent::{Agent, AgentError, Decision};
use crate::client::LlmClient;
use crate::registry::ToolRegistry;
use crate::schema_simplifier;
use crate::types::Message;
use crate::union_schema;

/// Agent for models without native structured output or function calling.
pub struct FlexibleAgent<C: LlmClient> {
    client: C,
    system_prompt: String,
    /// Maximum parse retry attempts (1 = no retry, 5 = up to 5 attempts).
    max_retries: usize,
}

impl<C: LlmClient> FlexibleAgent<C> {
    pub fn new(client: C, system_prompt: impl Into<String>, max_retries: usize) -> Self {
        Self {
            client,
            system_prompt: system_prompt.into(),
            max_retries: max_retries.max(1),
        }
    }
}

/// Build tool descriptions for system prompt using SchemaSimplifier.
fn tools_prompt(tools: &ToolRegistry) -> String {
    let mut s = String::from(
        "## Available Tools\n\nRespond with JSON: {\"situation\": \"...\", \"task\": [...], \"actions\": [{\"tool_name\": \"...\", ...args}]}\n\n",
    );
    for t in tools.list() {
        s.push_str(&schema_simplifier::simplify_tool(
            t.name(),
            t.description(),
            &t.parameters_schema(),
        ));
        s.push_str("\n\n");
    }
    s
}

/// Generate a format error correction prompt with accumulated errors.
fn format_error_prompt(errors: &[String]) -> String {
    let mut prompt = String::from(
        "Your previous response(s) could not be parsed as valid JSON. Please fix and try again.\n\nErrors:\n",
    );
    for (i, err) in errors.iter().enumerate() {
        prompt.push_str(&format!("{}. {}\n", i + 1, err));
    }
    prompt.push_str(
        "\nRespond with ONLY valid JSON matching the schema. No markdown, no explanations.",
    );
    prompt
}

#[async_trait::async_trait]
impl<C: LlmClient> Agent for FlexibleAgent<C> {
    async fn decide(
        &self,
        messages: &[Message],
        tools: &ToolRegistry,
    ) -> Result<Decision, AgentError> {
        let defs = tools.to_defs();

        // Build system prompt with tool descriptions
        let full_system = format!("{}\n\n{}", self.system_prompt, tools_prompt(tools));
        let mut msgs = Vec::with_capacity(messages.len() + 1);
        let has_system = messages
            .iter()
            .any(|m| m.role == crate::types::Role::System);
        if !has_system {
            msgs.push(Message::system(&full_system));
        }
        msgs.extend_from_slice(messages);

        let mut errors: Vec<String> = Vec::new();

        for attempt in 0..self.max_retries {
            // On retry, add error feedback
            if attempt > 0 && !errors.is_empty() {
                msgs.push(Message::user(format_error_prompt(&errors)));
            }

            let raw = self.client.complete(&msgs).await?;

            match union_schema::parse_action(&raw, &defs) {
                Ok((situation, tool_calls)) => {
                    let completed = tool_calls.is_empty()
                        || tool_calls.iter().any(|tc| tc.name == "finish_task");
                    return Ok(Decision {
                        situation,
                        task: vec![],
                        tool_calls,
                        completed,
                    });
                }
                Err(e) => {
                    errors.push(e.to_string());
                    // Add the raw response as assistant message for context
                    msgs.push(Message::assistant(&raw));
                }
            }
        }

        // All retries exhausted — treat last raw response as completed
        Ok(Decision {
            situation: format!(
                "Failed to parse after {} attempts. Errors: {}",
                self.max_retries,
                errors.join("; ")
            ),
            task: vec![],
            tool_calls: vec![],
            completed: true,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_tool::{ToolError, ToolOutput};
    use crate::client::LlmClient;
    use crate::context::AgentContext;
    use crate::tool::ToolDef;
    use crate::types::{SgrError, ToolCall};
    use serde_json::Value;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct MockTextClient {
        response: String,
    }

    #[async_trait::async_trait]
    impl LlmClient for MockTextClient {
        async fn structured_call(
            &self,
            _: &[Message],
            _: &Value,
        ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
            Ok((None, vec![], String::new()))
        }
        async fn tools_call(
            &self,
            _: &[Message],
            _: &[ToolDef],
        ) -> Result<Vec<ToolCall>, SgrError> {
            Ok(vec![])
        }
        async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
            Ok(self.response.clone())
        }
    }

    struct DummyTool;

    #[async_trait::async_trait]
    impl crate::agent_tool::Tool for DummyTool {
        fn name(&self) -> &str {
            "search"
        }
        fn description(&self) -> &str {
            "search files"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    #[tokio::test]
    async fn flexible_agent_parses_json_from_text() {
        let client = MockTextClient {
            response: r#"Sure, let me search for that.
```json
{"situation": "searching", "task": ["find files"], "actions": [{"tool_name": "search", "query": "main.rs"}]}
```"#
            .into(),
        };
        let agent = FlexibleAgent::new(client, "You are a test agent", 1);
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("find main.rs")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert_eq!(decision.tool_calls.len(), 1);
        assert_eq!(decision.tool_calls[0].name, "search");
    }

    #[tokio::test]
    async fn flexible_agent_plain_text_completes() {
        let client = MockTextClient {
            response: "I can't find any tools to use here.".into(),
        };
        let agent = FlexibleAgent::new(client, "test", 1);
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("hello")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert!(decision.completed);
        assert!(decision.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn flexible_agent_retry_succeeds() {
        /// Client that fails first, succeeds second
        struct RetryClient {
            call_count: Arc<AtomicUsize>,
        }
        #[async_trait::async_trait]
        impl LlmClient for RetryClient {
            async fn structured_call(
                &self,
                _: &[Message],
                _: &Value,
            ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
                Ok((None, vec![], String::new()))
            }
            async fn tools_call(
                &self,
                _: &[Message],
                _: &[ToolDef],
            ) -> Result<Vec<ToolCall>, SgrError> {
                Ok(vec![])
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                let n = self.call_count.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Ok("not valid json at all".into())
                } else {
                    Ok(
                        r#"{"situation": "found it", "task": [], "actions": [{"tool_name": "search", "query": "test"}]}"#
                            .into(),
                    )
                }
            }
        }

        let client = RetryClient {
            call_count: Arc::new(AtomicUsize::new(0)),
        };
        let agent = FlexibleAgent::new(client, "test", 3);
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("search")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert_eq!(decision.tool_calls.len(), 1);
        assert_eq!(decision.situation, "found it");
    }

    #[tokio::test]
    async fn flexible_agent_retry_exhausted() {
        let client = MockTextClient {
            response: "garbage output always".into(),
        };
        let agent = FlexibleAgent::new(client, "test", 3);
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("do something")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert!(decision.completed);
        assert!(decision.tool_calls.is_empty());
        assert!(
            decision
                .situation
                .contains("Failed to parse after 3 attempts")
        );
    }

    #[test]
    fn format_error_prompt_content() {
        let errors = vec!["bad json".to_string(), "missing field".to_string()];
        let prompt = format_error_prompt(&errors);
        assert!(prompt.contains("1. bad json"));
        assert!(prompt.contains("2. missing field"));
        assert!(prompt.contains("valid JSON"));
    }

    #[test]
    fn tools_prompt_uses_simplifier() {
        let tools = ToolRegistry::new().register(DummyTool);
        let prompt = tools_prompt(&tools);
        assert!(prompt.contains("### search"));
        assert!(prompt.contains("search files"));
    }
}
