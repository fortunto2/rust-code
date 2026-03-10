//! Tool registry — ordered collection of tools with lookup and fuzzy resolve.

use crate::agent_tool::Tool;
use crate::tool::ToolDef;
use indexmap::IndexMap;

/// Ordered registry of tools. Builder pattern for registration.
pub struct ToolRegistry {
    tools: IndexMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: IndexMap::new() }
    }

    /// Register a tool. Builder pattern (chainable).
    pub fn register(mut self, tool: impl Tool + 'static) -> Self {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
        self
    }

    /// Add a tool (mutable, non-chainable).
    pub fn add(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name().to_string(), Box::new(tool));
    }

    /// Get tool by name (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        let lower = name.to_lowercase();
        self.tools
            .iter()
            .find(|(k, _)| k.to_lowercase() == lower)
            .map(|(_, v)| v.as_ref())
    }

    /// List all tools (insertion order).
    pub fn list(&self) -> Vec<&dyn Tool> {
        self.tools.values().map(|t| t.as_ref()).collect()
    }

    /// List system tools only.
    pub fn system_tools(&self) -> Vec<&dyn Tool> {
        self.tools.values().filter(|t| t.is_system()).map(|t| t.as_ref()).collect()
    }

    /// Convert all tools to ToolDef for LLM API.
    pub fn to_defs(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| t.to_def()).collect()
    }

    /// Fuzzy resolve: exact match first, then Levenshtein distance.
    pub fn resolve(&self, name: &str) -> Option<&dyn Tool> {
        // Exact (case-insensitive)
        if let Some(t) = self.get(name) {
            return Some(t);
        }
        // Fuzzy
        let lower = name.to_lowercase();
        let mut best: Option<(&str, f64)> = None;
        for key in self.tools.keys() {
            let score = strsim::normalized_levenshtein(&lower, &key.to_lowercase());
            if score > 0.6 {
                if best.is_none() || score > best.unwrap().1 {
                    best = Some((key.as_str(), score));
                }
            }
        }
        best.and_then(|(k, _)| self.tools.get(k).map(|t| t.as_ref()))
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_tool::{ToolError, ToolOutput};
    use crate::context::AgentContext;
    use serde_json::Value;

    struct MockTool {
        tool_name: String,
        desc: String,
        system: bool,
    }

    impl MockTool {
        fn new(name: &str, desc: &str) -> Self {
            Self { tool_name: name.into(), desc: desc.into(), system: false }
        }
        fn system(name: &str, desc: &str) -> Self {
            Self { tool_name: name.into(), desc: desc.into(), system: true }
        }
    }

    #[async_trait::async_trait]
    impl Tool for MockTool {
        fn name(&self) -> &str {
            &self.tool_name
        }
        fn description(&self) -> &str {
            &self.desc
        }
        fn is_system(&self) -> bool {
            self.system
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(&self, _: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::text("ok"))
        }
    }

    #[test]
    fn registry_builder() {
        let reg = ToolRegistry::new()
            .register(MockTool::new("read_file", "Read a file"))
            .register(MockTool::new("write_file", "Write a file"));
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn registry_get_case_insensitive() {
        let reg =
            ToolRegistry::new().register(MockTool::new("ReadFile", "Read"));
        assert!(reg.get("readfile").is_some());
        assert!(reg.get("READFILE").is_some());
        assert!(reg.get("ReadFile").is_some());
    }

    #[test]
    fn registry_list_preserves_order() {
        let reg = ToolRegistry::new()
            .register(MockTool::new("alpha", "a"))
            .register(MockTool::new("beta", "b"))
            .register(MockTool::new("gamma", "c"));
        let names: Vec<_> = reg.list().iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn registry_system_tools() {
        let reg = ToolRegistry::new()
            .register(MockTool::new("read_file", "Read"))
            .register(MockTool::system("finish", "Finish task"));
        let sys = reg.system_tools();
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0].name(), "finish");
    }

    #[test]
    fn registry_to_defs() {
        let reg = ToolRegistry::new()
            .register(MockTool::new("bash", "Run command"));
        let defs = reg.to_defs();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "bash");
    }

    #[test]
    fn registry_fuzzy_resolve() {
        let reg = ToolRegistry::new()
            .register(MockTool::new("read_file", "Read"))
            .register(MockTool::new("write_file", "Write"));
        // Exact
        assert_eq!(reg.resolve("read_file").unwrap().name(), "read_file");
        // Fuzzy (typo)
        assert_eq!(reg.resolve("reed_file").unwrap().name(), "read_file");
        // Too different
        assert!(reg.resolve("xyz").is_none());
    }

    #[test]
    fn registry_add_mutable() {
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("tool_a", "A"));
        reg.add(MockTool::new("tool_b", "B"));
        assert_eq!(reg.len(), 2);
    }
}
