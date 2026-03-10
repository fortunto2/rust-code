//! Built-in system tools for interactive agents.
//!
//! - `ClarificationTool` — pause the loop to ask the user a question
//! - `PlanTool` — submit a structured implementation plan

use crate::agent_tool::{Tool, ToolError, ToolOutput};
use crate::context::AgentContext;
use serde_json::Value;

/// Ask the user a clarifying question. Pauses the agent loop until the user responds.
///
/// When used with `run_loop_interactive`, the loop calls `on_input(question)` and
/// injects the user's response as the tool result. With plain `run_loop`, emits
/// `LoopEvent::WaitingForInput` and continues with a placeholder.
pub struct ClarificationTool;

#[async_trait::async_trait]
impl Tool for ClarificationTool {
    fn name(&self) -> &str {
        "ask_user"
    }
    fn description(&self) -> &str {
        "Ask the user a clarifying question when you need more information to proceed"
    }
    fn is_system(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user"
                }
            },
            "required": ["question"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let question = args
            .get("question")
            .and_then(|v| v.as_str())
            .unwrap_or("Could you provide more details?");
        Ok(ToolOutput::waiting(question))
    }
}

/// Submit a structured implementation plan.
///
/// The plan is stored in `ctx.custom["plan"]` for retrieval after the loop completes.
/// Signals `done` — the planning phase is complete.
///
/// Expected args:
/// ```json
/// {
///   "summary": "Add user authentication",
///   "steps": [
///     { "description": "Create auth module", "files": ["src/auth.rs"], "tool_hints": ["write_file"] },
///     { "description": "Add tests", "files": ["tests/auth.rs"] }
///   ]
/// }
/// ```
pub struct PlanTool;

#[async_trait::async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str {
        "submit_plan"
    }
    fn description(&self) -> &str {
        "Submit your implementation plan after analyzing the codebase. Call when ready to present the plan."
    }
    fn is_system(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "Brief summary of what the plan achieves"
                },
                "steps": {
                    "type": "array",
                    "description": "Ordered list of implementation steps",
                    "items": {
                        "type": "object",
                        "properties": {
                            "description": {
                                "type": "string",
                                "description": "What this step does"
                            },
                            "files": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Files to create or modify"
                            },
                            "tool_hints": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Tools likely needed for this step"
                            }
                        },
                        "required": ["description"]
                    }
                }
            },
            "required": ["summary", "steps"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let summary = args
            .get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("Plan submitted")
            .to_string();
        // Store the full plan in context
        ctx.set("plan", args);
        Ok(ToolOutput::done(format!("Plan submitted: {summary}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn clarification_returns_waiting() {
        let tool = ClarificationTool;
        let mut ctx = AgentContext::new();
        let args = serde_json::json!({"question": "Which database?"});
        let output = tool.execute(args, &mut ctx).await.unwrap();
        assert!(output.waiting);
        assert!(!output.done);
        assert_eq!(output.content, "Which database?");
    }

    #[tokio::test]
    async fn clarification_default_question() {
        let tool = ClarificationTool;
        let mut ctx = AgentContext::new();
        let output = tool.execute(serde_json::json!({}), &mut ctx).await.unwrap();
        assert!(output.waiting);
        assert!(output.content.contains("more details"));
    }

    #[tokio::test]
    async fn plan_tool_stores_and_completes() {
        let tool = PlanTool;
        let mut ctx = AgentContext::new();
        let args = serde_json::json!({
            "summary": "Add auth",
            "steps": [
                {"description": "Create module", "files": ["src/auth.rs"]},
                {"description": "Add tests"}
            ]
        });
        let output = tool.execute(args, &mut ctx).await.unwrap();
        assert!(output.done);
        assert!(output.content.contains("Add auth"));

        // Plan stored in context
        let plan = ctx.get("plan").unwrap();
        assert_eq!(plan["steps"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn clarification_is_system_tool() {
        assert!(ClarificationTool.is_system());
    }

    #[test]
    fn plan_is_system_tool() {
        assert!(PlanTool.is_system());
    }
}
