//! PlanningAgent — read-only agent variant that produces structured plans.
//!
//! Wraps any `Agent` impl and restricts tools to a read-only subset.
//! The agent explores the codebase, then calls `submit_plan` with a structured plan.
//!
//! # Usage
//!
//! ```ignore
//! let inner = SgrAgent::new(client, PLAN_SYSTEM_PROMPT);
//! let planner = PlanningAgent::new(Box::new(inner));
//!
//! // Register read-only tools + PlanTool
//! let tools = ToolRegistry::new()
//!     .register(ReadFile)
//!     .register(ListDir)
//!     .register(SearchCode)
//!     .register(PlanTool)
//!     .register(ClarificationTool);
//!
//! run_loop(&planner, &tools, &mut ctx, &mut msgs, &config, |e| { ... }).await?;
//!
//! // Extract the plan
//! let plan: Plan = Plan::from_context(&ctx).unwrap();
//! ```

use crate::agent::{Agent, AgentError, Decision};
use crate::context::AgentContext;
use crate::registry::ToolRegistry;
use crate::types::Message;
use serde_json::Value;

/// A structured plan produced by the PlanningAgent.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Plan {
    pub summary: String,
    pub steps: Vec<PlanStep>,
}

/// A single step in the plan.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanStep {
    pub description: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub tool_hints: Vec<String>,
}

impl Plan {
    /// Extract plan from AgentContext (set by PlanTool).
    pub fn from_context(ctx: &AgentContext) -> Option<Self> {
        ctx.get("plan")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Convert plan to a message for injection into a build agent's context.
    pub fn to_message(&self) -> Message {
        let mut text = format!("## Implementation Plan\n\n{}\n\n", self.summary);
        for (i, step) in self.steps.iter().enumerate() {
            text.push_str(&format!("{}. {}\n", i + 1, step.description));
            if !step.files.is_empty() {
                text.push_str(&format!("   Files: {}\n", step.files.join(", ")));
            }
        }
        Message::system(&text)
    }
}

/// Tool names that are safe for read-only plan mode.
pub const READ_ONLY_TOOLS: &[&str] = &[
    "read_file",
    "list_files",
    "list_dir",
    "search",
    "search_code",
    "grep",
    "glob",
    "git_status",
    "git_diff",
    "git_log",
    "get_cwd",
    "change_dir",
    // System tools (always allowed)
    "ask_user",
    "submit_plan",
    "finish_task",
];

/// Wraps any Agent to enforce read-only tool access for planning.
///
/// Filters tools via `prepare_tools` to only allow read-only operations.
/// Sets `plan_mode: true` in context so tools can check and adapt behavior.
pub struct PlanningAgent {
    inner: Box<dyn Agent>,
    allowed_tools: Vec<String>,
}

impl PlanningAgent {
    pub fn new(inner: Box<dyn Agent>) -> Self {
        Self {
            inner,
            allowed_tools: READ_ONLY_TOOLS.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Override the set of allowed tools (replaces default READ_ONLY_TOOLS).
    pub fn with_allowed_tools(mut self, tools: Vec<String>) -> Self {
        self.allowed_tools = tools;
        self
    }

    /// Add extra tools to the allowed set (e.g. custom read-only tools).
    pub fn allow_tool(mut self, name: impl Into<String>) -> Self {
        self.allowed_tools.push(name.into());
        self
    }
}

#[async_trait::async_trait]
impl Agent for PlanningAgent {
    async fn decide(
        &self,
        messages: &[Message],
        tools: &ToolRegistry,
    ) -> Result<Decision, AgentError> {
        self.inner.decide(messages, tools).await
    }

    fn prepare_tools(&self, _ctx: &AgentContext, tools: &ToolRegistry) -> Vec<String> {
        tools
            .list()
            .iter()
            .filter(|t| {
                t.is_system()
                    || self
                        .allowed_tools
                        .iter()
                        .any(|a| a.eq_ignore_ascii_case(t.name()))
            })
            .map(|t| t.name().to_string())
            .collect()
    }

    fn prepare_context(&self, ctx: &mut AgentContext, messages: &[Message]) {
        ctx.set("plan_mode", Value::Bool(true));
        self.inner.prepare_context(ctx, messages);
    }

    fn after_action(&self, ctx: &mut AgentContext, tool_name: &str, output: &str) {
        self.inner.after_action(ctx, tool_name, output);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_tool::{Tool, ToolError, ToolOutput};
    use crate::registry::ToolRegistry;

    // Mock agent that returns one tool call then completes
    struct MockAgent;

    #[async_trait::async_trait]
    impl Agent for MockAgent {
        async fn decide(&self, _: &[Message], _: &ToolRegistry) -> Result<Decision, AgentError> {
            Ok(Decision {
                situation: "planning".into(),
                task: vec![],
                tool_calls: vec![],
                completed: true,
            })
        }
    }

    struct ReadFileTool;
    #[async_trait::async_trait]
    impl Tool for ReadFileTool {
        fn name(&self) -> &str {
            "read_file"
        }
        fn description(&self) -> &str {
            "read"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("content"))
        }
    }

    struct WriteFileTool;
    #[async_trait::async_trait]
    impl Tool for WriteFileTool {
        fn name(&self) -> &str {
            "write_file"
        }
        fn description(&self) -> &str {
            "write"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("written"))
        }
    }

    struct BashTool;
    #[async_trait::async_trait]
    impl Tool for BashTool {
        fn name(&self) -> &str {
            "bash"
        }
        fn description(&self) -> &str {
            "bash"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("output"))
        }
    }

    #[test]
    fn planning_filters_write_tools() {
        let planner = PlanningAgent::new(Box::new(MockAgent));
        let tools = ToolRegistry::new()
            .register(ReadFileTool)
            .register(WriteFileTool)
            .register(BashTool);

        let ctx = AgentContext::new();
        let allowed = planner.prepare_tools(&ctx, &tools);

        assert!(allowed.contains(&"read_file".to_string()));
        assert!(!allowed.contains(&"write_file".to_string()));
        assert!(!allowed.contains(&"bash".to_string()));
    }

    #[test]
    fn planning_sets_plan_mode_in_context() {
        let planner = PlanningAgent::new(Box::new(MockAgent));
        let mut ctx = AgentContext::new();
        let msgs = vec![Message::user("plan this")];

        planner.prepare_context(&mut ctx, &msgs);
        assert_eq!(ctx.get("plan_mode"), Some(&Value::Bool(true)));
    }

    #[test]
    fn plan_from_context() {
        let mut ctx = AgentContext::new();
        ctx.set(
            "plan",
            serde_json::json!({
                "summary": "Add auth",
                "steps": [
                    {"description": "Create module", "files": ["src/auth.rs"]},
                    {"description": "Write tests"}
                ]
            }),
        );

        let plan = Plan::from_context(&ctx).unwrap();
        assert_eq!(plan.summary, "Add auth");
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.steps[0].files, vec!["src/auth.rs"]);
        assert!(plan.steps[1].files.is_empty());
    }

    #[test]
    fn plan_to_message() {
        let plan = Plan {
            summary: "Refactor auth".into(),
            steps: vec![
                PlanStep {
                    description: "Extract trait".into(),
                    files: vec!["src/auth.rs".into()],
                    tool_hints: vec![],
                },
                PlanStep {
                    description: "Add tests".into(),
                    files: vec![],
                    tool_hints: vec![],
                },
            ],
        };
        let msg = plan.to_message();
        assert!(msg.content.contains("Refactor auth"));
        assert!(msg.content.contains("1. Extract trait"));
        assert!(msg.content.contains("src/auth.rs"));
    }

    #[test]
    fn allow_extra_tools() {
        let planner = PlanningAgent::new(Box::new(MockAgent)).allow_tool("custom_search");

        let tools = ToolRegistry::new()
            .register(ReadFileTool)
            .register(WriteFileTool);

        let ctx = AgentContext::new();
        let allowed = planner.prepare_tools(&ctx, &tools);
        assert!(allowed.contains(&"read_file".to_string()));
        // custom_search not in registry, so not in result
        // but if it were, it would be allowed
    }
}
