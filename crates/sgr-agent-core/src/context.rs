//! Agent execution context — state and domain-specific data.

use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;

/// Well-known key in `AgentContext.custom` for max_tokens override.
pub const MAX_TOKENS_OVERRIDE_KEY: &str = "_max_tokens_override";

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
    pub iteration: usize,
    pub state: AgentState,
    pub cwd: PathBuf,
    pub custom: HashMap<String, Value>,
    pub tool_configs: HashMap<String, Value>,
    pub writable_roots: Vec<PathBuf>,
    pub observations: Vec<String>,
    pub observation_limit: usize,
    pub tool_cache: HashMap<String, String>,
}

impl AgentContext {
    pub fn new() -> Self {
        Self {
            iteration: 0,
            state: AgentState::Running,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            custom: HashMap::new(),
            tool_configs: HashMap::new(),
            writable_roots: Vec::new(),
            observations: Vec::new(),
            observation_limit: 30,
            tool_cache: HashMap::new(),
        }
    }

    pub fn observe(&mut self, entry: impl Into<String>) {
        self.observations.push(entry.into());
        while self.observations.len() > self.observation_limit {
            self.observations.remove(0);
        }
    }

    pub fn observation_summary(&self) -> Option<String> {
        if self.observations.is_empty() {
            None
        } else {
            Some(format!(
                "OBSERVATION LOG:\n{}",
                self.observations.join("\n")
            ))
        }
    }

    pub fn cache_tool_result(&mut self, key: impl Into<String>, result: impl Into<String>) {
        self.tool_cache.insert(key.into(), result.into());
    }

    pub fn cached_tool_result(&self, key: &str) -> Option<&str> {
        self.tool_cache.get(key).map(|s| s.as_str())
    }

    pub fn invalidate_cache(&mut self, key: &str) {
        self.tool_cache.remove(key);
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    pub fn with_writable_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.writable_roots = roots;
        self
    }

    pub fn is_writable(&self, path: &std::path::Path) -> bool {
        if self.writable_roots.is_empty() {
            return true;
        }
        let abs_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        let resolved = std::fs::canonicalize(&abs_path).unwrap_or_else(|_| {
            if let Some(parent) = abs_path.parent()
                && let Ok(canon_parent) = std::fs::canonicalize(parent)
                && let Some(name) = abs_path.file_name()
            {
                return canon_parent.join(name);
            }
            abs_path.clone()
        });
        self.writable_roots.iter().any(|root| {
            let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            resolved.starts_with(&canon_root)
        })
    }

    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.custom.insert(key.into(), value);
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.custom.get(key)
    }

    pub fn max_tokens_override(&self) -> Option<u32> {
        self.custom
            .get(MAX_TOKENS_OVERRIDE_KEY)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
    }

    pub fn set_tool_config(&mut self, tool_name: impl Into<String>, config: Value) {
        self.tool_configs.insert(tool_name.into(), config);
    }

    pub fn tool_config(&self, tool_name: &str) -> Option<&Value> {
        self.tool_configs.get(tool_name)
    }

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
    fn tool_config_merge() {
        let mut ctx = AgentContext::new();
        ctx.set_tool_config("bash", serde_json::json!({"timeout": 60, "shell": "zsh"}));
        let base = serde_json::json!({"timeout": 30, "cwd": "/tmp"});
        let merged = ctx.merged_tool_config("bash", &base);
        assert_eq!(merged["timeout"], 60);
        assert_eq!(merged["cwd"], "/tmp");
        assert_eq!(merged["shell"], "zsh");
    }
}
