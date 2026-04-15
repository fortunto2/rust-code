//! Tool registry — ordered collection of tools with lookup and fuzzy resolve.

use crate::agent_tool::Tool;
use crate::tool::ToolDef;
use indexmap::IndexMap;

/// Lightweight proxy tool for filtered registries.
/// Only used for schema generation (to_defs/list), not execution.
struct ProxyTool {
    def: ToolDef,
}

impl ProxyTool {
    fn from_def(def: ToolDef) -> Self {
        Self { def }
    }
}

#[async_trait::async_trait]
impl Tool for ProxyTool {
    fn name(&self) -> &str {
        &self.def.name
    }
    fn description(&self) -> &str {
        &self.def.description
    }
    fn parameters_schema(&self) -> serde_json::Value {
        self.def.parameters.clone()
    }
    async fn execute(
        &self,
        _: serde_json::Value,
        _: &mut crate::context::AgentContext,
    ) -> Result<crate::agent_tool::ToolOutput, crate::agent_tool::ToolError> {
        Err(crate::agent_tool::ToolError::Execution(
            "ProxyTool cannot execute — use the original registry".into(),
        ))
    }
}

/// Error from `ToolRegistry::resolve()`.
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// Tool exists but is deferred — schema not loaded yet.
    Deferred(String),
    /// Tool not found (not active, not deferred, no fuzzy match).
    NotFound(String),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::Deferred(name) => write!(
                f,
                "Tool '{}' is deferred. Call tool_search to load its schema first.",
                name
            ),
            ResolveError::NotFound(name) => write!(f, "Tool '{}' not found.", name),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Ordered registry of tools. Builder pattern for registration.
///
/// Supports deferred tools — tools whose schema is hidden from the LLM until
/// explicitly promoted. Deferred tools appear in `to_defs()` with empty parameters,
/// and `resolve()` returns an error message directing the caller to load the schema first.
pub struct ToolRegistry {
    tools: IndexMap<String, Box<dyn Tool>>,
    /// Deferred tools: model sees name+description only, not full schema.
    /// Call `promote_deferred(name)` to move to active tools.
    deferred: IndexMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: IndexMap::new(),
            deferred: IndexMap::new(),
        }
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

    /// Register a deferred tool. Builder pattern (chainable).
    /// Deferred tools appear in `to_defs()` with empty parameters (name + description only).
    /// The LLM must call a search/load mechanism to promote them before use.
    pub fn register_deferred(mut self, tool: impl Tool + 'static) -> Self {
        self.deferred
            .insert(tool.name().to_string(), Box::new(tool));
        self
    }

    /// Add a deferred tool (mutable, non-chainable).
    pub fn add_deferred(&mut self, tool: impl Tool + 'static) {
        self.deferred
            .insert(tool.name().to_string(), Box::new(tool));
    }

    /// Move a deferred tool to the active registry.
    /// Returns true if the tool was found and promoted, false if not found.
    pub fn promote_deferred(&mut self, name: &str) -> bool {
        let lower = name.to_lowercase();
        let key = self
            .deferred
            .keys()
            .find(|k| k.to_lowercase() == lower)
            .cloned();
        if let Some(key) = key
            && let Some(tool) = self.deferred.swap_remove(&key)
        {
            self.tools.insert(key, tool);
            return true;
        }
        false
    }

    /// List deferred tool names.
    pub fn deferred_names(&self) -> Vec<&str> {
        self.deferred.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a tool name is in the deferred set.
    pub fn is_deferred(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        self.deferred.keys().any(|k| k.to_lowercase() == lower)
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
        self.tools
            .values()
            .filter(|t| t.is_system())
            .map(|t| t.as_ref())
            .collect()
    }

    /// Convert all tools to ToolDef for LLM API.
    /// Active tools get full schema; deferred tools get name + description with empty parameters.
    pub fn to_defs(&self) -> Vec<ToolDef> {
        let mut defs: Vec<ToolDef> = self.tools.values().map(|t| t.to_def()).collect();
        // AI-NOTE: deferred tools emit stub schema — LLM sees them but can't call until promoted
        for tool in self.deferred.values() {
            defs.push(ToolDef {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {}}),
            });
        }
        defs
    }

    /// Core tools only (no deferred stubs). For single-phase mode where fewer tools = better parallel FC.
    pub fn core_defs(&self) -> Vec<ToolDef> {
        self.tools.values().map(|t| t.to_def()).collect()
    }

    /// Fuzzy resolve: exact match first, then Levenshtein distance.
    /// Returns `Err(message)` if the tool is deferred (schema not yet loaded).
    pub fn resolve(&self, name: &str) -> Result<&dyn Tool, ResolveError> {
        // Exact (case-insensitive)
        if let Some(t) = self.get(name) {
            return Ok(t);
        }
        // Check deferred before fuzzy — deferred is an exact match, not "not found"
        if self.is_deferred(name) {
            return Err(ResolveError::Deferred(name.to_string()));
        }
        // Fuzzy
        let lower = name.to_lowercase();
        let mut best: Option<(&str, f64)> = None;
        for key in self.tools.keys() {
            let score = strsim::normalized_levenshtein(&lower, &key.to_lowercase());
            if score > 0.6 && (best.is_none() || score > best.unwrap().1) {
                best = Some((key.as_str(), score));
            }
        }
        match best.and_then(|(k, _)| self.tools.get(k).map(|t| t.as_ref())) {
            Some(t) => Ok(t),
            None => Err(ResolveError::NotFound(name.to_string())),
        }
    }

    /// Number of registered tools.
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Create a filtered view containing only tools with the given names.
    /// Preserves insertion order of the original registry.
    pub fn filter(&self, names: &[String]) -> ToolRegistry {
        // We can't move tools, so we create a registry that references
        // the same tools by name. Since ToolRegistry owns Box<dyn Tool>,
        // we need to create a new registry with references.
        // Instead, we return a ToolRegistry with only matching defs.
        // For agent_loop usage, we construct a lightweight proxy.
        self.clone_filtered(names)
    }

    /// Clone registry keeping only named tools (via wrapper structs).
    fn clone_filtered(&self, names: &[String]) -> ToolRegistry {
        let mut new_tools = IndexMap::new();
        for name in names {
            let lower = name.to_lowercase();
            for (k, v) in &self.tools {
                if k.to_lowercase() == lower {
                    // We can't clone Box<dyn Tool> generically, so we wrap
                    // the tool def as a passthrough. For the agent loop's
                    // tool execution, we always use the original registry.
                    new_tools.insert(k.clone(), ProxyTool::from_def(v.to_def()));
                }
            }
        }
        let mut reg = ToolRegistry::new();
        for (_, tool) in new_tools {
            reg.tools.insert(tool.def.name.clone(), Box::new(tool));
        }
        reg
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
            Self {
                tool_name: name.into(),
                desc: desc.into(),
                system: false,
            }
        }
        fn system(name: &str, desc: &str) -> Self {
            Self {
                tool_name: name.into(),
                desc: desc.into(),
                system: true,
            }
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
        let reg = ToolRegistry::new().register(MockTool::new("ReadFile", "Read"));
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
        let reg = ToolRegistry::new().register(MockTool::new("bash", "Run command"));
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
        assert!(reg.resolve("xyz").is_err());
    }

    #[test]
    fn registry_add_mutable() {
        let mut reg = ToolRegistry::new();
        reg.add(MockTool::new("tool_a", "A"));
        reg.add(MockTool::new("tool_b", "B"));
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn register_deferred_adds_to_deferred_map() {
        let reg = ToolRegistry::new()
            .register(MockTool::new("active", "Active tool"))
            .register_deferred(MockTool::new("lazy", "Lazy tool"));
        assert_eq!(reg.len(), 1); // only active tools counted
        assert!(reg.is_deferred("lazy"));
        assert!(!reg.is_deferred("active"));
        assert_eq!(reg.deferred_names(), vec!["lazy"]);
    }

    #[test]
    fn to_defs_returns_deferred_with_empty_params() {
        let reg = ToolRegistry::new()
            .register(MockTool::new("active", "Active tool"))
            .register_deferred(MockTool::new("lazy", "Lazy tool"));
        let defs = reg.to_defs();
        assert_eq!(defs.len(), 2);
        // Active tool has real schema
        let active_def = defs.iter().find(|d| d.name == "active").unwrap();
        assert!(active_def.parameters["type"] == "object");
        // Deferred tool has empty properties
        let lazy_def = defs.iter().find(|d| d.name == "lazy").unwrap();
        assert_eq!(lazy_def.description, "Lazy tool");
        assert_eq!(lazy_def.parameters["properties"], serde_json::json!({}));
    }

    #[test]
    fn promote_deferred_moves_to_active() {
        let mut reg = ToolRegistry::new().register_deferred(MockTool::new("lazy", "Lazy tool"));
        assert_eq!(reg.len(), 0);
        assert!(reg.is_deferred("lazy"));

        let promoted = reg.promote_deferred("lazy");
        assert!(promoted);
        assert_eq!(reg.len(), 1);
        assert!(!reg.is_deferred("lazy"));
        assert!(reg.get("lazy").is_some());
    }

    #[test]
    fn promote_deferred_not_found() {
        let mut reg = ToolRegistry::new();
        assert!(!reg.promote_deferred("ghost"));
    }

    #[test]
    fn resolve_deferred_returns_error() {
        let reg = ToolRegistry::new()
            .register(MockTool::new("active", "Active"))
            .register_deferred(MockTool::new("lazy", "Lazy"));
        // Active resolves fine
        assert!(reg.resolve("active").is_ok());
        // Deferred returns specific error
        let result = reg.resolve("lazy");
        assert!(result.is_err());
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("expected Deferred error"),
        };
        assert!(matches!(err, ResolveError::Deferred(_)));
        assert!(err.to_string().contains("tool_search"));
    }

    #[test]
    fn resolve_deferred_after_promote() {
        let mut reg = ToolRegistry::new().register_deferred(MockTool::new("lazy", "Lazy"));
        assert!(reg.resolve("lazy").is_err());
        reg.promote_deferred("lazy");
        assert!(reg.resolve("lazy").is_ok());
    }
}
