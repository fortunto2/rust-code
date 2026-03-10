//! HybridAgent — 2-phase agent (reasoning + action).
//!
//! Phase 1: Send a minimal ReasoningTool-only FC call to get the agent's
//! reasoning about what to do next.
//! Phase 2: Send the full toolkit FC call with the reasoning as context,
//! getting back concrete tool calls.
//!
//! Inspired by Python SGRToolCallingAgent — separates "thinking" from "acting"
//! so the model doesn't get overwhelmed by a large tool set during reasoning.

use crate::agent::{Agent, AgentError, Decision};
use crate::client::LlmClient;
use crate::registry::ToolRegistry;
use crate::types::Message;

/// 2-phase hybrid agent.
pub struct HybridAgent<C: LlmClient> {
    client: C,
    system_prompt: String,
}

impl<C: LlmClient> HybridAgent<C> {
    pub fn new(client: C, system_prompt: impl Into<String>) -> Self {
        Self {
            client,
            system_prompt: system_prompt.into(),
        }
    }
}

/// Internal reasoning tool definition for phase 1.
fn reasoning_tool_def() -> crate::tool::ToolDef {
    crate::tool::ToolDef {
        name: "reasoning".to_string(),
        description: "Analyze the situation and decide what tools to use next. Describe your reasoning, the current situation, and which tools you plan to call.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "situation": {
                    "type": "string",
                    "description": "Your assessment of the current situation"
                },
                "plan": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Step-by-step plan of what to do next"
                },
                "done": {
                    "type": "boolean",
                    "description": "Set to true if the task is fully complete"
                }
            },
            "required": ["situation", "plan", "done"]
        }),
    }
}

#[async_trait::async_trait]
impl<C: LlmClient> Agent for HybridAgent<C> {
    async fn decide(
        &self,
        messages: &[Message],
        tools: &ToolRegistry,
    ) -> Result<Decision, AgentError> {
        // Prepare messages with system prompt
        let mut msgs = Vec::with_capacity(messages.len() + 1);
        let has_system = messages.iter().any(|m| m.role == crate::types::Role::System);
        if !has_system && !self.system_prompt.is_empty() {
            msgs.push(Message::system(&self.system_prompt));
        }
        msgs.extend_from_slice(messages);

        // Phase 1: Reasoning — FC call with only the reasoning tool
        let reasoning_defs = vec![reasoning_tool_def()];
        let reasoning_calls = self.client.tools_call(&msgs, &reasoning_defs).await?;

        // Extract reasoning from phase 1
        let (situation, plan, done) = if let Some(rc) = reasoning_calls.first() {
            let sit = rc
                .arguments
                .get("situation")
                .and_then(|s| s.as_str())
                .unwrap_or("")
                .to_string();
            let plan: Vec<String> = rc
                .arguments
                .get("plan")
                .and_then(|p| p.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            let done = rc
                .arguments
                .get("done")
                .and_then(|d| d.as_bool())
                .unwrap_or(false);
            (sit, plan, done)
        } else {
            // No reasoning call — treat as completed
            return Ok(Decision {
                situation: String::new(),
                task: vec![],
                tool_calls: vec![],
                completed: true,
            });
        };

        // If reasoning says done, complete without phase 2
        if done {
            return Ok(Decision {
                situation,
                task: plan,
                tool_calls: vec![],
                completed: true,
            });
        }

        // Phase 2: Action — FC call with full toolkit + reasoning context
        let mut action_msgs = msgs.clone();
        // Add reasoning as assistant context
        let reasoning_context = format!(
            "Reasoning: {}\nPlan: {}",
            situation,
            plan.join(", ")
        );
        action_msgs.push(Message::assistant(&reasoning_context));
        // Prompt to execute
        action_msgs.push(Message::user(
            "Now execute the next step from your plan using the available tools.",
        ));

        let defs = tools.to_defs();
        let tool_calls = self.client.tools_call(&action_msgs, &defs).await?;

        let completed = tool_calls.is_empty()
            || tool_calls.iter().any(|tc| tc.name == "finish_task");

        Ok(Decision {
            situation,
            task: plan,
            tool_calls,
            completed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_tool::{Tool, ToolError, ToolOutput};
    use crate::context::AgentContext;
    use crate::tool::ToolDef;
    use crate::types::{SgrError, ToolCall};
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Mock client that returns reasoning in phase 1, tool call in phase 2.
    struct MockHybridClient {
        call_count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl LlmClient for MockHybridClient {
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
            _tools: &[ToolDef],
        ) -> Result<Vec<ToolCall>, SgrError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                // Phase 1: reasoning
                Ok(vec![ToolCall {
                    id: "r1".into(),
                    name: "reasoning".into(),
                    arguments: serde_json::json!({
                        "situation": "Need to read a file",
                        "plan": ["read main.rs", "analyze contents"],
                        "done": false
                    }),
                }])
            } else {
                // Phase 2: action
                Ok(vec![ToolCall {
                    id: "a1".into(),
                    name: "read_file".into(),
                    arguments: serde_json::json!({"path": "main.rs"}),
                }])
            }
        }
        async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
            Ok(String::new())
        }
    }

    struct DummyTool;
    #[async_trait::async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str { "read_file" }
        fn description(&self) -> &str { "read a file" }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("file contents"))
        }
    }

    #[tokio::test]
    async fn hybrid_two_phases() {
        let client = MockHybridClient {
            call_count: Arc::new(AtomicUsize::new(0)),
        };
        let agent = HybridAgent::new(client, "test agent");
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("read main.rs")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert_eq!(decision.situation, "Need to read a file");
        assert_eq!(decision.task.len(), 2);
        assert_eq!(decision.tool_calls.len(), 1);
        assert_eq!(decision.tool_calls[0].name, "read_file");
        assert!(!decision.completed);
    }

    #[tokio::test]
    async fn hybrid_done_in_reasoning() {
        struct DoneClient;
        #[async_trait::async_trait]
        impl LlmClient for DoneClient {
            async fn structured_call(&self, _: &[Message], _: &Value)
                -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
                Ok((None, vec![], String::new()))
            }
            async fn tools_call(&self, _: &[Message], _: &[ToolDef])
                -> Result<Vec<ToolCall>, SgrError> {
                Ok(vec![ToolCall {
                    id: "r1".into(),
                    name: "reasoning".into(),
                    arguments: serde_json::json!({
                        "situation": "Task is already complete",
                        "plan": [],
                        "done": true
                    }),
                }])
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                Ok(String::new())
            }
        }

        let agent = HybridAgent::new(DoneClient, "test");
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("done")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert!(decision.completed);
        assert!(decision.tool_calls.is_empty());
    }

    #[tokio::test]
    async fn hybrid_no_reasoning_completes() {
        struct EmptyClient;
        #[async_trait::async_trait]
        impl LlmClient for EmptyClient {
            async fn structured_call(&self, _: &[Message], _: &Value)
                -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
                Ok((None, vec![], String::new()))
            }
            async fn tools_call(&self, _: &[Message], _: &[ToolDef])
                -> Result<Vec<ToolCall>, SgrError> {
                Ok(vec![])
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                Ok(String::new())
            }
        }

        let agent = HybridAgent::new(EmptyClient, "test");
        let tools = ToolRegistry::new().register(DummyTool);
        let msgs = vec![Message::user("hello")];

        let decision = agent.decide(&msgs, &tools).await.unwrap();
        assert!(decision.completed);
    }
}
