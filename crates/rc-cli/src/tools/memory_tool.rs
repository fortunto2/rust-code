//! Memory tool — save/forget agent memory entries.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;
use std::path::Path;

const AGENT_HOME: &str = ".rust-code";

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct MemoryArgs {
    /// Operation: "save" or "forget".
    pub operation: String,
    /// Category: insight, pattern, decision, preference, debug.
    #[serde(default)]
    pub category: Option<String>,
    /// Section name.
    #[serde(default)]
    pub section: Option<String>,
    /// Memory content.
    #[serde(default)]
    pub content: Option<String>,
    /// Context for this memory.
    #[serde(default)]
    pub context: Option<String>,
    /// Confidence: "confirmed" or "tentative".
    #[serde(default)]
    pub confidence: Option<String>,
}

pub struct MemoryTool;

#[async_trait::async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str {
        "memory"
    }
    fn description(&self) -> &str {
        "Save or forget an agent memory entry."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<MemoryArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: MemoryArgs = parse_args(&args)?;
        let memory_path = Path::new(AGENT_HOME).join("MEMORY.jsonl");
        let op = args.operation.to_lowercase();
        let cat = args.category.as_deref().unwrap_or("insight").to_lowercase();
        let conf = args
            .confidence
            .as_deref()
            .unwrap_or("tentative")
            .to_lowercase();

        match op.as_str() {
            "save" => {
                let sec = args.section.as_deref().unwrap_or("general");
                let entry = serde_json::json!({
                    "category": cat,
                    "section": sec,
                    "content": args.content,
                    "context": args.context,
                    "confidence": conf,
                    "created": std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default().as_secs(),
                });
                let mut file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&memory_path)
                    .map_err(|e| ToolError::Execution(format!("Memory write: {}", e)))?;
                use std::io::Write;
                writeln!(file, "{}", entry)
                    .map_err(|e| ToolError::Execution(format!("Memory write: {}", e)))?;
                Ok(ToolOutput::text(format!(
                    "Memory saved: [{}] {} ({})",
                    cat, sec, conf
                )))
            }
            "forget" => {
                let sec = args.section.as_deref().unwrap_or("general");
                if memory_path.exists() {
                    let file_content = std::fs::read_to_string(&memory_path).unwrap_or_default();
                    let filtered: Vec<&str> = file_content
                        .lines()
                        .filter(|line| {
                            serde_json::from_str::<serde_json::Value>(line)
                                .map(|v| v["section"].as_str() != Some(sec))
                                .unwrap_or(true)
                        })
                        .collect();
                    let removed = file_content.lines().count() - filtered.len();
                    std::fs::write(&memory_path, filtered.join("\n") + "\n")
                        .map_err(|e| ToolError::Execution(format!("Memory write: {}", e)))?;
                    Ok(ToolOutput::text(format!(
                        "Memory: forgot {} entries from '{}'",
                        removed, sec
                    )))
                } else {
                    Ok(ToolOutput::text("Memory: nothing to forget (no entries)"))
                }
            }
            _ => Ok(ToolOutput::text(format!(
                "Unknown memory operation: {}",
                op
            ))),
        }
    }
}
