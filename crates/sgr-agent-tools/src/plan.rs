//! PlanTool — task checklist for LLM agents (Codex-compatible).
//!
//! The LLM calls `update_plan` to record its progress as a structured checklist.
//! The tool itself does nothing — it's the INPUT that matters. Clients (TUI, API)
//! read the plan from AgentContext and render it.
//!
//! ```json
//! {
//!   "plan": [
//!     {"step": "Read config file", "status": "completed"},
//!     {"step": "Fix the bug", "status": "in_progress"},
//!     {"step": "Run tests", "status": "pending"}
//!   ]
//! }
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

/// A single step in the agent's plan.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanStep {
    /// Description of the step.
    pub step: String,
    /// Status: "pending", "in_progress", or "completed".
    pub status: String,
}

/// Current plan state — stored in AgentContext typed store.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanState {
    pub steps: Vec<PlanStep>,
    pub explanation: Option<String>,
}

impl PlanState {
    /// Summary for display: "3/5 steps done"
    pub fn summary(&self) -> String {
        let done = self
            .steps
            .iter()
            .filter(|s| s.status == "completed")
            .count();
        let total = self.steps.len();
        let current = self.steps.iter().find(|s| s.status == "in_progress");
        match current {
            Some(s) => format!("{done}/{total} done — {}", s.step),
            None if done == total && total > 0 => format!("{done}/{total} done"),
            _ => format!("{done}/{total} steps"),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct UpdatePlanArgs {
    /// Optional explanation of the plan or current thinking.
    #[serde(default)]
    explanation: Option<String>,
    /// The list of steps with status.
    plan: Vec<PlanStep>,
}

/// Checklist tool — LLM records its task plan for UI rendering.
///
/// Stores plan in `AgentContext` typed store as `PlanState`.
/// Returns "Plan updated" — the value is in the structured input, not the output.
pub struct UpdatePlanTool;

#[async_trait::async_trait]
impl Tool for UpdatePlanTool {
    fn name(&self) -> &str {
        "update_plan"
    }
    fn description(&self) -> &str {
        "Update the task plan checklist. Provide steps with status (pending/in_progress/completed). \
         At most one step should be in_progress at a time. Call this to track your progress."
    }
    fn is_system(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<UpdatePlanArgs>()
    }

    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: UpdatePlanArgs = parse_args(&args)?;
        let state = PlanState {
            steps: a.plan,
            explanation: a.explanation,
        };
        ctx.insert(state.clone());
        Ok(ToolOutput::text(format!(
            "Plan updated: {}",
            state.summary()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_update_plan() {
        let tool = UpdatePlanTool;
        let mut ctx = AgentContext::new();

        let result = tool
            .execute(
                serde_json::json!({
                    "plan": [
                        {"step": "Read file", "status": "completed"},
                        {"step": "Fix bug", "status": "in_progress"},
                        {"step": "Run tests", "status": "pending"}
                    ]
                }),
                &mut ctx,
            )
            .await
            .unwrap();

        assert!(result.content.contains("1/3 done"));
        assert!(result.content.contains("Fix bug"));

        let state = ctx.get_typed::<PlanState>().unwrap();
        assert_eq!(state.steps.len(), 3);
        assert_eq!(state.steps[0].status, "completed");
    }

    #[test]
    fn plan_summary() {
        let state = PlanState {
            steps: vec![
                PlanStep {
                    step: "A".into(),
                    status: "completed".into(),
                },
                PlanStep {
                    step: "B".into(),
                    status: "completed".into(),
                },
                PlanStep {
                    step: "C".into(),
                    status: "pending".into(),
                },
            ],
            explanation: None,
        };
        assert_eq!(state.summary(), "2/3 steps");
    }
}
