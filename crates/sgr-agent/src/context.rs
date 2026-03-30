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
    /// Directories the agent is allowed to write to (sandbox).
    /// Empty = no restriction.
    pub writable_roots: Vec<PathBuf>,
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
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    pub fn with_writable_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.writable_roots = roots;
        self
    }

    /// Check if a path is writable under sandbox rules.
    /// Returns true if writable_roots is empty (no sandbox) or path is under any root.
    /// Canonicalizes paths to prevent traversal attacks (../ and symlinks).
    pub fn is_writable(&self, path: &std::path::Path) -> bool {
        if self.writable_roots.is_empty() {
            return true;
        }
        let abs_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        // Canonicalize to resolve ".." and symlinks.
        // If the path doesn't exist yet (new file), canonicalize the parent.
        let resolved = std::fs::canonicalize(&abs_path).unwrap_or_else(|_| {
            // File doesn't exist — canonicalize parent, then append filename
            if let Some(parent) = abs_path.parent()
                && let Ok(canon_parent) = std::fs::canonicalize(parent)
                && let Some(name) = abs_path.file_name()
            {
                return canon_parent.join(name);
            }
            abs_path.clone()
        });
        self.writable_roots.iter().any(|root| {
            // Canonicalize the root too (resolve symlinks in root paths)
            let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
            resolved.starts_with(&canon_root)
        })
    }

    /// Set a custom value.
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.custom.insert(key.into(), value);
    }

    /// Get a custom value.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.custom.get(key)
    }

    /// Get max_tokens override set by a tool's ContextModifier.
    /// Returns None if no override is set. Agents can call this in `prepare_context`
    /// to adjust LlmConfig.max_tokens before the next model call.
    pub fn max_tokens_override(&self) -> Option<u32> {
        self.custom
            .get(crate::agent_tool::MAX_TOKENS_OVERRIDE_KEY)
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
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

    #[test]
    fn writable_roots_empty_allows_all() {
        let ctx = AgentContext::new();
        assert!(ctx.is_writable(std::path::Path::new("/any/path")));
    }

    #[test]
    fn writable_roots_restricts() {
        let ctx =
            AgentContext::new().with_writable_roots(vec![PathBuf::from("/home/user/project")]);
        assert!(ctx.is_writable(std::path::Path::new("/home/user/project/src/main.rs")));
        assert!(!ctx.is_writable(std::path::Path::new("/etc/passwd")));
    }

    #[test]
    fn writable_roots_relative_path() {
        let ctx = AgentContext::new()
            .with_cwd("/home/user/project")
            .with_writable_roots(vec![PathBuf::from("/home/user/project")]);
        assert!(ctx.is_writable(std::path::Path::new("src/main.rs")));
        assert!(!ctx.is_writable(std::path::Path::new("/etc/passwd")));
    }
}
