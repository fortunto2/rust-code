//! Tool completion hooks — generic workflow guidance for SGR agents.
//!
//! After a tool executes, hooks check if the action matches a pattern
//! and return messages to inject before the next LLM decision.
//!
//! sgr-agent provides the primitives:
//! - `Hook`: trigger pattern → message
//! - `HookRegistry`: stores hooks, matches against tool actions
//! - `SgrAgent::after_execute`: trait method wired into app_loop
//!
//! Each project populates the registry with its own hooks
//! (parsed from config, project docs, or hardcoded).

use std::sync::Arc;

/// A single completion hook: when tool + path match → inject message.
#[derive(Clone, Debug)]
pub struct Hook {
    /// Tool name to match ("write", "read", "delete", "*" for any)
    pub tool: String,
    /// Path substring to match (lowercase, e.g. "distill/cards/")
    pub path_contains: String,
    /// Path substrings to exclude from matching (e.g. "template")
    pub exclude: Vec<String>,
    /// Message to inject after tool completes
    pub message: String,
}

/// Registry of tool hooks.
#[derive(Clone, Debug, Default)]
pub struct HookRegistry {
    hooks: Vec<Hook>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self { hooks: Vec::new() }
    }

    /// Add a hook to the registry.
    pub fn add(&mut self, hook: Hook) -> &mut Self {
        self.hooks.push(hook);
        self
    }

    /// Check if a tool action matches any hook.
    /// Returns messages to inject (empty if no match).
    pub fn check(&self, tool_name: &str, path: &str) -> Vec<String> {
        let norm = path.trim_start_matches('/').to_lowercase();
        self.hooks
            .iter()
            .filter(|h| {
                (h.tool == tool_name || h.tool == "*")
                    && norm.contains(&h.path_contains)
                    && !h.exclude.iter().any(|ex| norm.contains(ex))
            })
            .map(|h| h.message.clone())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.hooks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.hooks.is_empty()
    }
}

/// Shared hook registry (Arc for multi-component access).
pub type Shared = Arc<HookRegistry>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_tool_and_path() {
        let mut reg = HookRegistry::new();
        reg.add(Hook {
            tool: "write".into(),
            path_contains: "cards/".into(),
            exclude: vec!["template".into()],
            message: "Update thread".into(),
        });

        assert_eq!(
            reg.check("write", "distill/cards/article.md"),
            vec!["Update thread"]
        );
        assert!(reg.check("write", "distill/cards/_template.md").is_empty());
        assert!(reg.check("read", "distill/cards/article.md").is_empty());
        assert!(reg.check("write", "contacts/john.json").is_empty());
    }

    #[test]
    fn wildcard_tool() {
        let mut reg = HookRegistry::new();
        reg.add(Hook {
            tool: "*".into(),
            path_contains: "inbox/".into(),
            exclude: vec![],
            message: "Inbox touched".into(),
        });

        assert_eq!(reg.check("read", "inbox/msg.txt"), vec!["Inbox touched"]);
        assert_eq!(reg.check("delete", "inbox/msg.txt"), vec!["Inbox touched"]);
    }

    #[test]
    fn empty_registry() {
        let reg = HookRegistry::new();
        assert!(reg.check("write", "anything").is_empty());
        assert!(reg.is_empty());
    }
}
