//! SgrAgent — structured output agent.
//!
//! Builds a discriminated union schema from the ToolRegistry, sends it via
//! `structured_call`, parses the response into tool calls using `parse_action`.

use crate::agent::{Agent, AgentError, Decision};
use crate::client::LlmClient;
use crate::registry::ToolRegistry;
use crate::types::Message;
use crate::union_schema;

/// Agent that uses structured output (JSON Schema) for tool selection.
pub struct SgrAgent<C: LlmClient> {
    client: C,
    system_prompt: String,
}

impl<C: LlmClient> SgrAgent<C> {
    pub fn new(client: C, system_prompt: impl Into<String>) -> Self {
        Self { client, system_prompt: system_prompt.into() }
    }
}

#[async_trait::async_trait]
impl<C: LlmClient> Agent for SgrAgent<C> {
    async fn decide(
        &self,
        messages: &[Message],
        tools: &ToolRegistry,
    ) -> Result<Decision, AgentError> {
        let defs = tools.to_defs();
        let schema = union_schema::build_action_schema(&defs);

        // Prepend system prompt if not already in messages
        let mut msgs = Vec::with_capacity(messages.len() + 1);
        let has_system = messages.iter().any(|m| m.role == crate::types::Role::System);
        if !has_system && !self.system_prompt.is_empty() {
            msgs.push(Message::system(&self.system_prompt));
        }
        msgs.extend_from_slice(messages);

        let (output, native_calls, raw) =
            self.client.structured_call(&msgs, &schema).await?;

        // Try to parse structured output first
        if let Some(val) = output {
            if let Ok((situation, tool_calls)) =
                union_schema::parse_action(&val.to_string(), &defs)
            {
                let completed = tool_calls.is_empty()
                    || tool_calls.iter().any(|tc| tc.name == "finish_task");
                return Ok(Decision {
                    situation,
                    task: vec![],
                    tool_calls,
                    completed,
                });
            }
        }

        // Fall back to native tool calls
        if !native_calls.is_empty() {
            let completed = native_calls.iter().any(|tc| tc.name == "finish_task");
            return Ok(Decision {
                situation: String::new(),
                task: vec![],
                tool_calls: native_calls,
                completed,
            });
        }

        // Try parsing raw text
        if let Ok((situation, tool_calls)) = union_schema::parse_action(&raw, &defs) {
            let completed =
                tool_calls.is_empty() || tool_calls.iter().any(|tc| tc.name == "finish_task");
            return Ok(Decision { situation, task: vec![], tool_calls, completed });
        }

        // No tool calls — completed
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
    use crate::context::AgentContext;
    use crate::tool::ToolDef;
    use crate::types::{SgrError, ToolCall};
    use serde_json::Value;

    struct MockClient {
        response: String,
    }

    #[async_trait::async_trait]
    impl LlmClient for MockClient {
        async fn structured_call(
            &self,
            _messages: &[Message],
            _schema: &Value,
        ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
            let val: Value = serde_json::from_str(&self.response).unwrap_or(Value::Null);
            Ok((Some(val), vec![], self.response.clone()))
        }
        async fn tools_call(
            &self,
            _messages: &[Message],
            _tools: &[ToolDef],
        ) -> Result<Vec<ToolCall>, SgrError> {
            Ok(vec![])
        }
        async fn complete(&self, _messages: &[Message]) -> Result<String, SgrError> {
            Ok(self.response.clone())
        }
    }

    struct DummyTool(&'static str);

    #[async_trait::async_trait]
    impl crate::agent_tool::Tool for DummyTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "dummy"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {"arg": {"type": "string"}}})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    #[tokio::test]
    async fn sgr_agent_parses_structured_output() {
        let client = MockClient {
            response: r#"{"situation":"reading file","task":["read"],"actions":[{"tool_name":"read","arg":"main.rs"}]}"#.into(),
        };
        let agent = SgrAgent::new(client, "You are a test agent");
        let tools = ToolRegistry::new().register(DummyTool("read"));
        let msgs = vec![Message::user("read main.rs")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert_eq!(decision.situation, "reading file");
        assert_eq!(decision.tool_calls.len(), 1);
        assert_eq!(decision.tool_calls[0].name, "read");
        assert!(!decision.completed);
    }

    #[tokio::test]
    async fn sgr_agent_empty_actions_completes() {
        let client = MockClient {
            response: r#"{"situation":"done","task":[],"actions":[]}"#.into(),
        };
        let agent = SgrAgent::new(client, "test");
        let tools = ToolRegistry::new().register(DummyTool("read"));
        let msgs = vec![Message::user("done")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert!(decision.completed);
    }
}
