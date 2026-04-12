//! Agent execution context — shared state passed to tools during execution.

use serde_json::Value;
use std::any::{Any, TypeId};
use std::collections::{HashMap, VecDeque};
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

/// Well-known key for max_tokens override (legacy string-key compat).
pub const MAX_TOKENS_OVERRIDE_KEY: &str = "_max_tokens_override";

/// Shared context passed to tools during execution.
///
/// Two ways to store custom state:
/// - **Typed** (preferred): `ctx.insert::<MyState>(state)` / `ctx.get_typed::<MyState>()`
/// - **String-keyed** (legacy): `ctx.set("key", json_value)` / `ctx.get("key")`
#[derive(Clone)]
pub struct AgentContext {
    pub iteration: usize,
    pub state: AgentState,
    pub cwd: PathBuf,
    /// String-keyed extensible state (legacy — prefer typed store).
    pub custom: HashMap<String, Value>,
    /// Per-tool configuration overrides.
    pub tool_configs: HashMap<String, Value>,
    /// Sandbox: writable directory roots (empty = no restriction).
    pub writable_roots: Vec<PathBuf>,
    /// Compressed observation log (FIFO, capped at `observation_limit`).
    observations: VecDeque<String>,
    pub observation_limit: usize,
    /// Tool result cache — keyed by "tool_name:arg_hash".
    pub tool_cache: HashMap<String, String>,
    /// Type-safe extensible store. Projects store typed data without string-key collisions.
    typed: HashMap<TypeId, TypedSlot>,
}

// -- Typed store support --

/// Wrapper that stores a concrete Clone + Send + Sync + 'static value as trait object.
struct TypedSlot {
    data: Box<dyn Any + Send + Sync>,
    clone_fn: fn(&Box<dyn Any + Send + Sync>) -> Box<dyn Any + Send + Sync>,
}

impl Clone for TypedSlot {
    fn clone(&self) -> Self {
        Self {
            data: (self.clone_fn)(&self.data),
            clone_fn: self.clone_fn,
        }
    }
}

fn make_clone_fn<T: Clone + Send + Sync + 'static>()
-> fn(&Box<dyn Any + Send + Sync>) -> Box<dyn Any + Send + Sync> {
    |data| {
        let val = data.downcast_ref::<T>().expect("TypedSlot type mismatch");
        Box::new(val.clone())
    }
}

impl std::fmt::Debug for AgentContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentContext")
            .field("iteration", &self.iteration)
            .field("state", &self.state)
            .field("cwd", &self.cwd)
            .field("custom_keys", &self.custom.keys().collect::<Vec<_>>())
            .field("typed_count", &self.typed.len())
            .field("observations", &self.observations.len())
            .finish()
    }
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
            observations: VecDeque::new(),
            observation_limit: 30,
            tool_cache: HashMap::new(),
            typed: HashMap::new(),
        }
    }

    // -- Typed store (preferred API) --

    /// Insert typed state. Each type T gets exactly one slot — no string-key collisions.
    ///
    /// ```rust,ignore
    /// #[derive(Clone)]
    /// struct MyToolState { count: usize }
    /// ctx.insert(MyToolState { count: 0 });
    /// ```
    pub fn insert<T: Clone + Send + Sync + 'static>(&mut self, value: T) {
        self.typed.insert(
            TypeId::of::<T>(),
            TypedSlot {
                data: Box::new(value),
                clone_fn: make_clone_fn::<T>(),
            },
        );
    }

    /// Get typed state by type.
    pub fn get_typed<T: Clone + Send + Sync + 'static>(&self) -> Option<&T> {
        self.typed
            .get(&TypeId::of::<T>())
            .and_then(|slot| slot.data.downcast_ref())
    }

    /// Remove typed state, returning it if present.
    pub fn remove_typed<T: Clone + Send + Sync + 'static>(&mut self) -> Option<T> {
        self.typed
            .remove(&TypeId::of::<T>())
            .and_then(|slot| slot.data.downcast::<T>().ok().map(|b| *b))
    }

    // -- Observations (VecDeque, O(1) eviction) --

    /// Record a compressed observation.
    pub fn observe(&mut self, entry: impl Into<String>) {
        self.observations.push_back(entry.into());
        while self.observations.len() > self.observation_limit {
            self.observations.pop_front();
        }
    }

    /// Get observation log as a single string for LLM context injection.
    pub fn observation_summary(&self) -> Option<String> {
        if self.observations.is_empty() {
            None
        } else {
            let joined: String = self
                .observations
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            Some(format!("OBSERVATION LOG:\n{joined}"))
        }
    }

    // -- Tool cache --

    pub fn cache_tool_result(&mut self, key: impl Into<String>, result: impl Into<String>) {
        self.tool_cache.insert(key.into(), result.into());
    }

    pub fn cached_tool_result(&self, key: &str) -> Option<&str> {
        self.tool_cache.get(key).map(|s| s.as_str())
    }

    pub fn invalidate_cache(&mut self, key: &str) {
        self.tool_cache.remove(key);
    }

    // -- Builders --

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    pub fn with_writable_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.writable_roots = roots;
        self
    }

    // -- Sandbox --

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

    // -- Legacy string-keyed custom data --

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

    // -- Per-tool config --

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

    #[test]
    fn typed_store() {
        #[derive(Clone, Debug, PartialEq)]
        struct MyState {
            count: usize,
        }

        let mut ctx = AgentContext::new();
        assert!(ctx.get_typed::<MyState>().is_none());

        ctx.insert(MyState { count: 42 });
        assert_eq!(ctx.get_typed::<MyState>().unwrap().count, 42);

        // Different types don't collide
        #[derive(Clone)]
        struct OtherState(#[allow(dead_code)] String);
        ctx.insert(OtherState("hello".into()));
        assert_eq!(ctx.get_typed::<MyState>().unwrap().count, 42);
    }

    #[test]
    fn typed_store_clone() {
        #[derive(Clone, PartialEq, Debug)]
        struct S(u32);

        let mut ctx = AgentContext::new();
        ctx.insert(S(7));

        let ctx2 = ctx.clone();
        assert_eq!(ctx2.get_typed::<S>().unwrap(), &S(7));
    }

    #[test]
    fn observations_fifo() {
        let mut ctx = AgentContext::new();
        ctx.observation_limit = 3;
        ctx.observe("a");
        ctx.observe("b");
        ctx.observe("c");
        ctx.observe("d"); // evicts "a"
        let summary = ctx.observation_summary().unwrap();
        assert!(!summary.contains("\na\n"));
        assert!(summary.contains("b"));
        assert!(summary.contains("d"));
    }
}
