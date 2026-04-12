//! ApplyPatch tool — edit files using patch format.

use crate::rc_state::RcState;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ApplyPatchArgs {
    /// Patch in the apply_patch format. Must start with "*** Begin Patch" and end with "*** End Patch".
    pub patch: String,
}

pub struct ApplyPatchTool {
    pub state: RcState,
}

#[async_trait::async_trait]
impl Tool for ApplyPatchTool {
    fn name(&self) -> &str {
        "apply_patch"
    }
    fn description(&self) -> &str {
        "Edit files. Use this for ALL file modifications. Format: '*** Begin Patch\\n*** Update File: path\\n@@ optional_context\\n context_line\\n-old_line\\n+new_line\\n*** End Patch'. Operations: Add/Delete/Update File. Lines prefixed with space (context), - (remove), + (add). Include 3 lines of context around changes."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<ApplyPatchArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ApplyPatchArgs = parse_args(&args)?;
        let current_cwd = self.state.cwd.lock().unwrap().clone();
        match sgr_agent::app_tools::apply_patch::apply_patch_to_files(&args.patch, &current_cwd)
            .await
        {
            Ok(result) => {
                let mut summary = String::new();
                for p in &result.added {
                    summary.push_str(&format!("A {}\n", p.display()));
                }
                for p in &result.modified {
                    summary.push_str(&format!("M {}\n", p.display()));
                }
                for p in &result.deleted {
                    summary.push_str(&format!("D {}\n", p.display()));
                }
                if summary.is_empty() {
                    summary = "Patch applied (no changes).".to_string();
                }

                // Show updated content so agent has fresh state for subsequent patches.
                // Limit: first 3 files, max 200 lines each.
                let changed: Vec<&std::path::Path> = result
                    .modified
                    .iter()
                    .chain(result.added.iter())
                    .take(3)
                    .map(|p| p.as_path())
                    .collect();
                for p in &changed {
                    let abs = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        current_cwd.join(p)
                    };
                    if let Ok(content) = tokio::fs::read_to_string(&abs).await {
                        let lines: Vec<&str> = content.lines().collect();
                        let display = if lines.len() > 200 {
                            format!(
                                "{}\n... ({} more lines)",
                                lines[..200].join("\n"),
                                lines.len() - 200
                            )
                        } else {
                            content
                        };
                        summary.push_str(&format!(
                            "\n--- Updated {} ---\n{}\n",
                            p.display(),
                            display
                        ));
                    }
                }

                // Invalidate read cache for changed files
                {
                    let mut cache = self.state.read_cache.lock().unwrap();
                    for p in result
                        .modified
                        .iter()
                        .chain(result.added.iter())
                        .chain(result.deleted.iter())
                    {
                        let key = p.to_string_lossy().to_string();
                        cache.remove(&key);
                        // Also remove with cwd prefix
                        let abs = current_cwd.join(p);
                        cache.remove(&abs.to_string_lossy().to_string());
                    }
                }

                Ok(ToolOutput::text(summary))
            }
            Err(e) => Err(ToolError::Execution(format!(
                "apply_patch error: {}\n\n\
                 IMPORTANT: If context lines don't match, use read_file FIRST to see the current file content, then retry.\n\n\
                 CORRECT FORMAT:\n\
                 *** Begin Patch\n\
                 *** Update File: path/to/file.ts\n\
                 @@ function_name\n\
                  context line (must match file exactly)\n\
                 -old line\n\
                 +new line\n\
                  context line\n\
                 *** End Patch\n\n\
                 Do NOT use unified diff (@@ -N,N +N,N @@). Use *** Add/Update/Delete File: headers.",
                e
            ))),
        }
    }
}
