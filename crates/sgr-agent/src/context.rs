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
}

impl AgentContext {
    pub fn new() -> Self {
        Self {
            iteration: 0,
            state: AgentState::Running,
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            custom: HashMap::new(),
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
}
