//! Task tool — manage tasks: create, list, update, done.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;
use std::path::Path;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct TaskArgs {
    /// Operation: create, list, update, done.
    pub operation: String,
    /// Task title (for create).
    #[serde(default)]
    pub title: Option<String>,
    /// Task ID (for update/done).
    #[serde(default)]
    pub task_id: Option<i64>,
    /// Status: todo, in_progress, blocked, done.
    #[serde(default)]
    pub status: Option<String>,
    /// Priority: low, medium, high.
    #[serde(default)]
    pub priority: Option<String>,
    /// Notes.
    #[serde(default)]
    pub notes: Option<String>,
}

pub struct TaskTool;

#[async_trait::async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }
    fn description(&self) -> &str {
        "Manage tasks: create, list, update, done."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<TaskArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: TaskArgs = parse_args(&args)?;
        let project_root = Path::new(".");
        let op = args.operation.to_lowercase();
        match op.as_str() {
            "create" => {
                let t = args.title.as_deref().unwrap_or("Untitled");
                let pri = args
                    .priority
                    .as_ref()
                    .and_then(|p| sgr_agent::Priority::parse(&p.to_lowercase()))
                    .unwrap_or(sgr_agent::Priority::Medium);
                let mut task = sgr_agent::create_task(project_root, t, pri);
                if let Some(n) = &args.notes {
                    task.body = n.clone();
                    sgr_agent::save_task(project_root, &task);
                }
                Ok(ToolOutput::text(format!(
                    "Created task #{} [{}] ({}): {}",
                    task.id, task.status, task.priority, task.title
                )))
            }
            "list" => {
                let tasks = sgr_agent::load_tasks(project_root);
                if tasks.is_empty() {
                    Ok(ToolOutput::text(
                        "No tasks found. Use task(operation='create') to create one.",
                    ))
                } else {
                    let mut output = format!("Tasks ({}):\n", tasks.len());
                    for t in &tasks {
                        output.push_str(&format!(
                            "  #{} [{}] ({}) {}\n",
                            t.id, t.status, t.priority, t.title
                        ));
                    }
                    Ok(ToolOutput::text(output))
                }
            }
            "update" => {
                let Some(id) = args.task_id else {
                    return Ok(ToolOutput::text("Error: task_id required for update"));
                };
                let id = id as u16;
                if let Some(status_val) = &args.status {
                    let status_str = status_val.to_lowercase();
                    if let Some(s) = sgr_agent::TaskStatus::parse(&status_str) {
                        sgr_agent::update_status(project_root, id, s);
                    }
                }
                if let Some(n) = &args.notes {
                    sgr_agent::append_notes(project_root, id, n);
                }
                let tasks = sgr_agent::load_tasks(project_root);
                let task = tasks.iter().find(|t| t.id == id);
                match task {
                    Some(t) => Ok(ToolOutput::text(format!(
                        "Updated task #{} [{}] ({}): {}",
                        t.id, t.status, t.priority, t.title
                    ))),
                    None => Ok(ToolOutput::text(format!("Task #{} not found", id))),
                }
            }
            "done" => {
                let Some(id) = args.task_id else {
                    return Ok(ToolOutput::text("Error: task_id required for done"));
                };
                match sgr_agent::update_status(project_root, id as u16, sgr_agent::TaskStatus::Done)
                {
                    Some(t) => Ok(ToolOutput::text(format!(
                        "Completed task #{}: {}",
                        t.id, t.title
                    ))),
                    None => Ok(ToolOutput::text(format!("Task #{} not found", id))),
                }
            }
            _ => Ok(ToolOutput::text(format!("Unknown task operation: {}", op))),
        }
    }
}
