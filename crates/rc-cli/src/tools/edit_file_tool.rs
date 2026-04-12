//! EditFile tool — simple single-string replacement (old_string -> new_string).

use crate::rc_state::RcState;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct EditFileArgs {
    /// File path to edit.
    pub path: String,
    /// Exact string to find and replace.
    pub old_string: String,
    /// Replacement string.
    pub new_string: String,
}

pub struct EditFileTool {
    pub state: RcState,
}

#[async_trait::async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str {
        "edit_file"
    }
    fn description(&self) -> &str {
        "DEPRECATED \u{2014} use apply_patch instead. Simple single-string replacement (old_string \u{2192} new_string)."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<EditFileArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: EditFileArgs = parse_args(&args)?;
        let resolved = self.state.resolve_path(&args.path);
        match crate::tools::edit_file(&resolved, &args.old_string, &args.new_string).await {
            Ok(()) => {
                // Reset failure counter and invalidate read cache on success
                self.state
                    .edit_failures
                    .lock()
                    .unwrap()
                    .remove(args.path.as_str());
                self.state.read_cache.lock().unwrap().remove(&resolved);
                let old_lines: Vec<&str> = args.old_string.lines().collect();
                let new_lines: Vec<&str> = args.new_string.lines().collect();
                let mut diff = format!(
                    "Edited {} ({}\u{2192}{} lines)\n",
                    args.path,
                    old_lines.len(),
                    new_lines.len()
                );
                for l in &old_lines {
                    diff.push_str(&format!("- {}\n", l));
                }
                for l in &new_lines {
                    diff.push_str(&format!("+ {}\n", l));
                }
                Ok(ToolOutput::text(diff))
            }
            Err(e) => {
                let count = {
                    let mut failures = self.state.edit_failures.lock().unwrap();
                    let c = failures.entry(args.path.to_string()).or_insert(0);
                    *c += 1;
                    *c
                };
                let mut err_msg = format!("{}", e);
                if count >= 2 {
                    err_msg.push_str(&format!(
                        "\n\n\u{26a0} edit_file has failed {} times on this file. \
                         STOP trying edit_file. Instead: use read_file to get the EXACT current content, \
                         then use write_file with the complete modified content.",
                        count
                    ));
                }
                Err(ToolError::Execution(err_msg))
            }
        }
    }
}
