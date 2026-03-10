//! Agent execution context — state and domain-specific data.

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// Agent execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Running,
    Completed,
    Failed,
    Cancelled,
    WaitingInput,
}

/// Shared context passed to tools during execution.
#[derive(Debug, Clone)]
pub struct AgentContext {
    /// Current iteration (step number).
    pub iteration: usize,
    /// Agent state.
    pub state: AgentState,
    /// Working directory.
    pub cwd: PathBuf,
    /// Domain-specific data (extensible).
    pub custom: HashMap<String, Value>,
    /// Per-tool configuration overrides.
    /// Key: tool name, Value: tool-specific config merged at execution time.
    pub tool_configs: HashMap<String, Value>,
}

impl AgentContext {
    pub fn new() -> Self {
        Self {
            iteration: 0,
            state: AgentState::Running,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            custom: HashMap::new(),
            tool_configs: HashMap::new(),
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// Set a custom value.
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.custom.insert(key.into(), value);
    }

    /// Get a custom value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.custom.get(key)
    }

    /// Set per-tool config.
    pub fn set_tool_config(&mut self, tool_name: impl Into<String>, config: Value) {
        self.tool_configs.insert(tool_name.into(), config);
    }

    /// Get per-tool config.
    pub fn tool_config(&self, tool_name: &str) -> Option<&Value> {
        self.tool_configs.get(tool_name)
    }

    /// Get tool config merged with a base config.
    /// Per-tool values override base values (shallow merge).
    pub fn merged_tool_config(&self, tool_name: &str, base: &Value) -> Value {
        match (base, self.tool_configs.get(tool_name)) {
            (Value::Object(base_obj), Some(Value::Object(override_obj))) => {
                let mut merged = base_obj.clone();
                for (k, v) in override_obj {
                    merged.insert(k.clone(), v.clone());
                }
                Value::Object(merged)
            }
            (_, Some(override_val)) => override_val.clone(),
            _ => base.clone(),
        }
    }
}

impl Default for AgentContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_default_state() {
        let ctx = AgentContext::new();
        assert_eq!(ctx.state, AgentState::Running);
        assert_eq!(ctx.iteration, 0);
    }

    #[test]
    fn context_custom_data() {
        let mut ctx = AgentContext::new();
        ctx.set("project", serde_json::json!("my-project"));
        assert_eq!(ctx.get("project").unwrap(), "my-project");
        assert!(ctx.get("missing").is_none());
    }

    #[test]
    fn context_with_cwd() {
        let ctx = AgentContext::new().with_cwd("/tmp/test");
        assert_eq!(ctx.cwd, PathBuf::from("/tmp/test"));
    }

    #[test]
    fn tool_config_set_get() {
        let mut ctx = AgentContext::new();
        ctx.set_tool_config("bash", serde_json::json!({"timeout": 30}));
        assert_eq!(ctx.tool_config("bash").unwrap()["timeout"], 30);
        assert!(ctx.tool_config("read_file").is_none());
    }

    #[test]
    fn tool_config_merge() {
        let mut ctx = AgentContext::new();
        ctx.set_tool_config("bash", serde_json::json!({"timeout": 60, "shell": "zsh"}));

        let base = serde_json::json!({"timeout": 30, "cwd": "/tmp"});
        let merged = ctx.merged_tool_config("bash", &base);
        // Override wins for timeout, base keeps cwd, override adds shell
        assert_eq!(merged["timeout"], 60);
        assert_eq!(merged["cwd"], "/tmp");
        assert_eq!(merged["shell"], "zsh");
    }

    #[test]
    fn tool_config_merge_no_override() {
        let ctx = AgentContext::new();
        let base = serde_json::json!({"timeout": 30});
        let merged = ctx.merged_tool_config("bash", &base);
        assert_eq!(merged, base);
    }
}
