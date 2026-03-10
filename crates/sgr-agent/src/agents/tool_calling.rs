//! ToolCallingAgent — uses native function calling (Gemini FC / OpenAI tools API).
//!
//! Sends tool definitions directly to the LLM's native function calling endpoint.
//! Simplest agent variant — no schema building, no parsing.

use crate::agent::{Agent, AgentError, Decision};
use crate::client::LlmClient;
use crate::registry::ToolRegistry;
use crate::types::Message;

/// Agent that uses native function calling.
pub struct ToolCallingAgent<C: LlmClient> {
    client: C,
    system_prompt: String,
}

impl<C: LlmClient> ToolCallingAgent<C> {
    pub fn new(client: C, system_prompt: impl Into<String>) -> Self {
        Self { client, system_prompt: system_prompt.into() }
    }
}

#[async_trait::async_trait]
impl<C: LlmClient> Agent for ToolCallingAgent<C> {
    async fn decide(
        &self,
        messages: &[Message],
        tools: &ToolRegistry,
    ) -> Result<Decision, AgentError> {
        let defs = tools.to_defs();

        let mut msgs = Vec::with_capacity(messages.len() + 1);
        let has_system = messages.iter().any(|m| m.role == crate::types::Role::System);
        if !has_system && !self.system_prompt.is_empty() {
            msgs.push(Message::system(&self.system_prompt));
        }
        msgs.extend_from_slice(messages);

        let tool_calls = self.client.tools_call(&msgs, &defs).await?;
        let completed =
            tool_calls.is_empty() || tool_calls.iter().any(|tc| tc.name == "finish_task");

        Ok(Decision {
            situation: String::new(),
            task: vec![],
            tool_calls,
            completed,
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

    struct MockFcClient {
        calls: Vec<ToolCall>,
    }

    #[async_trait::async_trait]
    impl LlmClient for MockFcClient {
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
            Ok(self.calls.clone())
        }
        async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
            Ok(String::new())
        }
    }

    struct DummyTool;

    #[async_trait::async_trait]
    impl crate::agent_tool::Tool for DummyTool {
        fn name(&self) -> &str { "bash" }
        fn description(&self) -> &str { "run command" }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    #[tokio::test]
    async fn tool_calling_agent_forwards_calls() {
        let client = MockFcClient {
            calls: vec![ToolCall {
                id: "1".into(),
                name: "bash".into(),
                arguments: serde_json::json!({"command": "ls"}),
            }],
        };
        let agent = ToolCallingAgent::new(client, "test");
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("list files")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert_eq!(decision.tool_calls.len(), 1);
        assert_eq!(decision.tool_calls[0].name, "bash");
        assert!(!decision.completed);
    }

    #[tokio::test]
    async fn tool_calling_agent_no_calls_completes() {
        let client = MockFcClient { calls: vec![] };
        let agent = ToolCallingAgent::new(client, "test");
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("done")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert!(decision.completed);
    }
}
