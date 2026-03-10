//! FlexibleAgent — text-based agent for weak models without structured output.
//!
//! Puts tool descriptions in the system prompt, sends plain completion,
//! then uses flexible_parser + coerce to extract tool calls from text.

use crate::agent::{Agent, AgentError, Decision};
use crate::client::LlmClient;
use crate::registry::ToolRegistry;
use crate::types::Message;
use crate::union_schema;

/// Agent for models without native structured output or function calling.
pub struct FlexibleAgent<C: LlmClient> {
    client: C,
    system_prompt: String,
}

impl<C: LlmClient> FlexibleAgent<C> {
    pub fn new(client: C, system_prompt: impl Into<String>) -> Self {
        Self { client, system_prompt: system_prompt.into() }
    }
}

/// Build tool descriptions for system prompt injection.
fn tools_prompt(tools: &ToolRegistry) -> String {
    let mut s = String::from("## Available Tools\n\nRespond with JSON: {\"situation\": \"...\", \"task\": [...], \"actions\": [{\"tool_name\": \"...\", ...args}]}\n\n");
    for t in tools.list() {
        s.push_str(&format!("### {}\n{}\nParameters: {}\n\n", t.name(), t.description(), t.parameters_schema()));
    }
    s
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
        let has_system = messages.iter().any(|m| m.role == crate::types::Role::System);
        if !has_system {
            msgs.push(Message::system(&full_system));
        }
        msgs.extend_from_slice(messages);

        // Plain completion
        let raw = self.client.complete(&msgs).await?;

        // Try to parse actions from text
        if let Ok((situation, tool_calls)) = union_schema::parse_action(&raw, &defs) {
            let completed =
                tool_calls.is_empty() || tool_calls.iter().any(|tc| tc.name == "finish_task");
            return Ok(Decision { situation, task: vec![], tool_calls, completed });
        }

        // Couldn't parse — treat as completed with text response
        Ok(Decision {
            situation: raw,
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
        fn name(&self) -> &str { "search" }
        fn description(&self) -> &str { "search files" }
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
```"#.into(),
        };
        let agent = FlexibleAgent::new(client, "You are a test agent");
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
        let agent = FlexibleAgent::new(client, "test");
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("hello")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert!(decision.completed);
        assert!(decision.tool_calls.is_empty());
    }
}
