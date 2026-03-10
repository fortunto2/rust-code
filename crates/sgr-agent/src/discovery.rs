//! Progressive discovery — filter tools by relevance to current query.
//!
//! Use `ToolFilter` when your registry has many tools (20+) and you want to
//! limit what the LLM sees per step. System tools are always included.
//! Non-system tools are ranked by keyword overlap + fuzzy matching (strsim).
//!
//! Usage: call `filter.select(user_query, &registry)` to get a subset of tools,
//! then pass those to your agent's `decide()` via a filtered registry.

use crate::agent_tool::Tool;
use crate::registry::ToolRegistry;

/// Tool filter for progressive discovery.
pub struct ToolFilter {
    /// Maximum number of non-system tools to expose.
    pub max_visible: usize,
}

impl ToolFilter {
    pub fn new(max_visible: usize) -> Self {
        Self { max_visible }
    }

    /// Select relevant tools for a query. System tools always included.
    pub fn select<'a>(&self, query: &str, registry: &'a ToolRegistry) -> Vec<&'a dyn Tool> {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut system_tools = Vec::new();
        let mut scored: Vec<(&dyn Tool, f64)> = Vec::new();

        for tool in registry.list() {
            if tool.is_system() {
                system_tools.push(tool);
                continue;
            }

            let score = score_tool(tool, &query_lower, &query_words);
            scored.push((tool, score));
        }

        // Sort by score descending
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Take top N
        let mut result = system_tools;
        for (tool, _score) in scored.into_iter().take(self.max_visible) {
            result.push(tool);
        }

        result
    }
}

impl Default for ToolFilter {
    fn default() -> Self {
        Self { max_visible: 10 }
    }
}

/// Score a tool's relevance to a query.
fn score_tool(tool: &dyn Tool, query_lower: &str, query_words: &[&str]) -> f64 {
    let name = tool.name().to_lowercase();
    let desc = tool.description().to_lowercase();
    let combined = format!("{} {}", name, desc);
    let tool_words: Vec<&str> = combined.split_whitespace().collect();

    let mut score = 0.0;

    // Exact name match
    if query_lower.contains(&name) {
        score += 5.0;
    }

    // Word intersection
    for qw in query_words {
        for tw in &tool_words {
            if qw == tw {
                score += 2.0;
            } else {
                let sim = strsim::normalized_levenshtein(qw, tw);
                if sim > 0.7 {
                    score += sim;
                }
            }
        }
    }

    // Substring match in name
    for qw in query_words {
        if name.contains(qw) {
            score += 1.5;
        }
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_tool::{ToolError, ToolOutput};
    use crate::context::AgentContext;
    use serde_json::Value;

    struct TestTool {
        tool_name: &'static str,
        desc: &'static str,
        system: bool,
    }

    #[async_trait::async_trait]
    impl Tool for TestTool {
        fn name(&self) -> &str { self.tool_name }
        fn description(&self) -> &str { self.desc }
        fn is_system(&self) -> bool { self.system }
        fn parameters_schema(&self) -> Value { serde_json::json!({"type": "object"}) }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    #[test]
    fn system_tools_always_included() {
        let reg = ToolRegistry::new()
            .register(TestTool { tool_name: "finish_task", desc: "finish", system: true })
            .register(TestTool { tool_name: "read_file", desc: "read a file from disk", system: false })
            .register(TestTool { tool_name: "bash", desc: "run shell command", system: false });

        let filter = ToolFilter::new(1);
        let selected = filter.select("read the file", &reg);

        // System tool always present
        assert!(selected.iter().any(|t| t.name() == "finish_task"));
        // Only 1 non-system tool (max_visible=1)
        let non_sys: Vec<_> = selected.iter().filter(|t| !t.is_system()).collect();
        assert_eq!(non_sys.len(), 1);
    }

    #[test]
    fn relevant_tool_ranked_higher() {
        let reg = ToolRegistry::new()
            .register(TestTool { tool_name: "read_file", desc: "read a file from disk", system: false })
            .register(TestTool { tool_name: "bash", desc: "run shell command", system: false })
            .register(TestTool { tool_name: "write_file", desc: "write content to a file", system: false });

        let filter = ToolFilter::new(2);
        let selected = filter.select("read the file main.rs", &reg);
        // read_file should be first non-system tool
        assert_eq!(selected[0].name(), "read_file");
    }

    #[test]
    fn empty_query_returns_all_up_to_max() {
        let reg = ToolRegistry::new()
            .register(TestTool { tool_name: "a", desc: "tool a", system: false })
            .register(TestTool { tool_name: "b", desc: "tool b", system: false })
            .register(TestTool { tool_name: "c", desc: "tool c", system: false });

        let filter = ToolFilter::new(2);
        let selected = filter.select("", &reg);
        assert_eq!(selected.len(), 2);
    }
}
