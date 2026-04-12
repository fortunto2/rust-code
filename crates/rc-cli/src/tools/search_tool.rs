//! SearchCode tool — ripgrep + fuzzy file path matching.

use crate::rc_state::RcState;
use crate::tools::{FuzzySearcher, truncate_output};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SearchCodeArgs {
    /// Search query (regex or text).
    pub query: String,
}

pub struct SearchCodeTool {
    pub state: RcState,
}

#[async_trait::async_trait]
impl Tool for SearchCodeTool {
    fn name(&self) -> &str {
        "search_code"
    }
    fn description(&self) -> &str {
        "Search codebase for a pattern using ripgrep."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<SearchCodeArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: SearchCodeArgs = parse_args(&args)?;
        let mut result = String::new();

        if let Ok(files) = FuzzySearcher::get_all_files().await {
            let mut searcher = FuzzySearcher::new();
            let matches = searcher.fuzzy_match_files(&args.query, &files);
            if !matches.is_empty() {
                result.push_str(&format!("File path matches for '{}':\n", args.query));
                for (score, path) in matches.iter().take(5) {
                    if *score > 50 {
                        result.push_str(&format!("- {}\n", path));
                    }
                }
                result.push('\n');
            }
        }

        result.push_str(&format!("Content search results for '{}':\n", args.query));
        let safe_query = args.query.replace("'", "'\\''");
        let search_cmd = format!("rg -n '{}' . || grep -rn '{}' .", safe_query, safe_query);
        let current_cwd = self.state.cwd.lock().unwrap().clone();
        let search_result = crate::tools::run_command_in(&search_cmd, &current_cwd, None).await;
        let output = &search_result.output;
        if search_result.exit_code == 0 && !output.trim().is_empty() {
            let lines: Vec<&str> = output.lines().collect();
            if lines.len() > 100 {
                result.push_str(&lines[..100].join("\n"));
                result.push_str(&format!(
                    "\n...[Truncated {} more lines]...",
                    lines.len() - 100
                ));
            } else {
                result.push_str(output);
            }
        } else {
            result.push_str("No content matches found.");
        }

        Ok(ToolOutput::text(truncate_output(&result)))
    }
}
