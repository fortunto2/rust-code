//! UpdatePlanTool — task checklist that persists to disk.
//!
//! Compatible with solo-factory `/plan` format (spec.md + plan.md).
//! LLM calls `update_plan` to record progress. Tool writes `plan.md` to cwd
//! and stores state in AgentContext typed store.
//!
//! Format: `- [x] completed` / `- [~] in progress` / `- [ ] pending`

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

impl PlanStep {
    fn checkbox(&self) -> &str {
        match self.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[~]",
            _ => "[ ]",
        }
    }
}

/// Current plan state — stored in AgentContext typed store.
/// Also written to `plan.md` in working directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlanState {
    pub steps: Vec<PlanStep>,
    pub explanation: Option<String>,
}

impl PlanState {
    /// Summary: "3/5 done — current step"
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

    /// Render as markdown checklist (solo-factory compatible).
    pub fn to_markdown(&self) -> String {
        let mut md = String::from("# Plan\n\n");
        if let Some(ref explanation) = self.explanation {
            md.push_str(&format!("{explanation}\n\n"));
        }
        md.push_str("## Tasks\n\n");
        for (i, step) in self.steps.iter().enumerate() {
            md.push_str(&format!(
                "- {} Task {}: {}\n",
                step.checkbox(),
                i + 1,
                step.step
            ));
        }
        md
    }
}

#[derive(Deserialize, JsonSchema)]
struct UpdatePlanArgs {
    /// Optional explanation of the plan or current thinking.
    #[serde(default)]
    explanation: Option<String>,
    /// The list of steps with status (pending/in_progress/completed).
    plan: Vec<PlanStep>,
}

/// Checklist tool — LLM records task plan, persisted to `plan.md`.
///
/// Stores in AgentContext typed store + writes to `{cwd}/plan.md`.
pub struct UpdatePlanTool;

#[async_trait::async_trait]
impl Tool for UpdatePlanTool {
    fn name(&self) -> &str {
        "update_plan"
    }
    fn description(&self) -> &str {
        "Update the task plan checklist. Provide steps with status (pending/in_progress/completed). \
         At most one step should be in_progress at a time. Plan is saved to plan.md."
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

        // Persist to disk
        let plan_path = ctx.cwd.join("plan.md");
        let md = state.to_markdown();
        std::fs::write(&plan_path, &md)
            .map_err(|e| ToolError::Execution(format!("write plan.md: {e}")))?;

        // Store in context for UI
        let summary = state.summary();
        ctx.insert(state);

        Ok(ToolOutput::text(format!(
            "Plan updated: {summary} (saved to plan.md)"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_update_plan() {
        let tool = UpdatePlanTool;
        let tmp = std::env::temp_dir().join("sgr_plan_test");
        let _ = std::fs::create_dir_all(&tmp);
        let mut ctx = AgentContext::new().with_cwd(&tmp);

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
        assert!(result.content.contains("plan.md"));

        // Check typed store
        let state = ctx.get_typed::<PlanState>().unwrap();
        assert_eq!(state.steps.len(), 3);

        // Check file on disk
        let md = std::fs::read_to_string(tmp.join("plan.md")).unwrap();
        assert!(md.contains("[x] Task 1: Read file"));
        assert!(md.contains("[~] Task 2: Fix bug"));
        assert!(md.contains("[ ] Task 3: Run tests"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn plan_to_markdown() {
        let state = PlanState {
            steps: vec![
                PlanStep {
                    step: "A".into(),
                    status: "completed".into(),
                },
                PlanStep {
                    step: "B".into(),
                    status: "in_progress".into(),
                },
                PlanStep {
                    step: "C".into(),
                    status: "pending".into(),
                },
            ],
            explanation: Some("Fix the auth bug".into()),
        };
        let md = state.to_markdown();
        assert!(md.contains("# Plan"));
        assert!(md.contains("Fix the auth bug"));
        assert!(md.contains("- [x] Task 1: A"));
        assert!(md.contains("- [~] Task 2: B"));
        assert!(md.contains("- [ ] Task 3: C"));
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
