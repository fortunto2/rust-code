//! Hint pipeline — tactical advice from multiple sources.
//!
//! Each hint source analyzes the current step context and emits
//! zero or more hints. All hints are collected and injected as
//! `HINT: ...` system messages before action execution.
//!
//! Sources:
//! 1. **Intent guard** — mode-based (Ask/Plan restrict mutations)
//! 2. **Pattern detector** — anti-patterns (edit-without-read, grep-loop, etc.)
//! 3. **Tool recommender** — suggest better tools for the task
//! 4. **Workflow rules** — TDD reminders, git discipline
//!
//! ```text
//! StepDecision { actions, situation, ... }
//!      │
//!      ├─→ intent_guard::guard_step()     → hints
//!      ├─→ pattern_hints()                → hints
//!      ├─→ tool_hints()                   → hints
//!      └─→ workflow_hints()               → hints
//!           │
//!           ▼
//!     deduplicated Vec<String> → HINT: system messages
//! ```

use crate::intent_guard::{self, ActionKind, Intent};

/// Context available to hint sources for a single step.
pub struct HintContext<'a> {
    /// Current user intent (Auto, Ask, Build, Plan).
    pub intent: Intent,
    /// Actions the agent wants to take this step.
    pub action_kinds: &'a [ActionKind],
    /// Step number (1-based).
    pub step_num: usize,
    /// Available MCP servers (for tool recommendations).
    pub mcp_servers: &'a [&'a str],
}

/// Trait for hint sources. Implement to add custom advice logic.
///
/// Built-in sources: `PatternHints`, `ToolHints`, `WorkflowHints`.
/// Projects can add their own (e.g. domain-specific rules).
pub trait HintSource: Send + Sync {
    /// Emit hints based on step context. Return empty vec if nothing to say.
    fn hints(&self, ctx: &HintContext) -> Vec<String>;
}

/// Built-in: detect anti-patterns (edit-without-read, grep-loop, etc.)
pub struct PatternHints;
/// Built-in: suggest better tools (MCP, etc.)
pub struct ToolHints;
/// Built-in: TDD reminders, git discipline.
pub struct WorkflowHints;

impl HintSource for PatternHints {
    fn hints(&self, ctx: &HintContext) -> Vec<String> {
        pattern_hints(ctx)
    }
}

impl HintSource for ToolHints {
    fn hints(&self, ctx: &HintContext) -> Vec<String> {
        tool_hints(ctx)
    }
}

impl HintSource for WorkflowHints {
    fn hints(&self, ctx: &HintContext) -> Vec<String> {
        workflow_hints(ctx)
    }
}

/// Default hint sources (all built-ins).
pub fn default_sources() -> Vec<Box<dyn HintSource>> {
    vec![
        Box::new(PatternHints),
        Box::new(ToolHints),
        Box::new(WorkflowHints),
    ]
}

/// Collect hints from intent guard + all provided sources. Deduplicates.
pub fn collect_hints<A>(
    ctx: &HintContext,
    actions: &[A],
    classify: impl Fn(&A) -> ActionKind,
    sources: &[Box<dyn HintSource>],
) -> Vec<String> {
    let mut hints = Vec::new();

    // Intent guard (always runs first)
    hints.extend(intent_guard::guard_step(ctx.intent, actions, &classify));

    // Pluggable sources
    for source in sources {
        hints.extend(source.hints(ctx));
    }

    // Deduplicate
    let mut seen = Vec::new();
    hints.retain(|h| {
        if seen.contains(h) {
            false
        } else {
            seen.push(h.clone());
            true
        }
    });

    hints
}

// ============================================================================
// Source 2: Pattern detector — anti-patterns in agent behavior
// ============================================================================

fn pattern_hints(ctx: &HintContext) -> Vec<String> {
    let mut hints = Vec::new();

    // Remind to read before writing on early steps
    let has_write = ctx
        .action_kinds
        .iter()
        .any(|k| matches!(k, ActionKind::Write));
    if has_write && ctx.step_num == 1 {
        hints.push(
            "Consider reading existing files before writing to avoid overwriting important code."
                .into(),
        );
    }

    hints
}

// ============================================================================
// Source 3: Tool recommender — suggest better tools
// ============================================================================

fn tool_hints(ctx: &HintContext) -> Vec<String> {
    let mut hints = Vec::new();

    // Suggest MCP tools when available
    let has_search = ctx
        .action_kinds
        .iter()
        .any(|k| matches!(k, ActionKind::Read));
    if has_search && ctx.mcp_servers.contains(&"codegraph") {
        // Only hint once, on first few steps
        if ctx.step_num <= 2 {
            hints.push(
                "codegraph MCP is available — project_code_search may be more accurate than grep."
                    .into(),
            );
        }
    }

    hints
}

// ============================================================================
// Source 4: Workflow rules — TDD, git discipline
// ============================================================================

fn workflow_hints(ctx: &HintContext) -> Vec<String> {
    let mut hints = Vec::new();

    // TDD reminder when writing code
    let has_write = ctx
        .action_kinds
        .iter()
        .any(|k| matches!(k, ActionKind::Write));
    if has_write {
        hints.push("Remember to run tests after writing code to verify changes.".into());
    }

    // Git discipline reminder on execute
    let has_execute = ctx
        .action_kinds
        .iter()
        .any(|k| matches!(k, ActionKind::Execute));
    if has_execute && ctx.step_num > 5 {
        hints.push("Consider committing progress if you haven't already.".into());
    }

    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_default<'a>() -> HintContext<'a> {
        HintContext {
            intent: Intent::Auto,
            action_kinds: &[],
            step_num: 1,
            mcp_servers: &[],
        }
    }

    #[test]
    fn no_hints_on_read_only() {
        let ctx = ctx_default();
        let actions: Vec<ActionKind> = vec![ActionKind::Read];
        let sources = default_sources();
        let hints = collect_hints(&ctx, &actions, |k| *k, &sources);
        assert!(hints.is_empty());
    }

    #[test]
    fn write_on_step_1_gets_read_reminder() {
        let ctx = HintContext {
            action_kinds: &[ActionKind::Write],
            step_num: 1,
            ..ctx_default()
        };
        let hints = pattern_hints(&ctx);
        assert!(hints.iter().any(|h| h.contains("reading existing files")));
    }

    #[test]
    fn write_gets_tdd_reminder() {
        let ctx = HintContext {
            action_kinds: &[ActionKind::Write],
            ..ctx_default()
        };
        let hints = workflow_hints(&ctx);
        assert!(hints.iter().any(|h| h.contains("tests")));
    }

    #[test]
    fn mcp_suggestion() {
        let ctx = HintContext {
            action_kinds: &[ActionKind::Read],
            mcp_servers: &["codegraph"],
            step_num: 1,
            ..ctx_default()
        };
        let hints = tool_hints(&ctx);
        assert!(hints.iter().any(|h| h.contains("codegraph")));
    }

    #[test]
    fn no_mcp_suggestion_on_late_steps() {
        let ctx = HintContext {
            action_kinds: &[ActionKind::Read],
            mcp_servers: &["codegraph"],
            step_num: 5,
            ..ctx_default()
        };
        let hints = tool_hints(&ctx);
        assert!(hints.is_empty());
    }

    #[test]
    fn git_reminder_on_late_execute() {
        let ctx = HintContext {
            action_kinds: &[ActionKind::Execute],
            step_num: 7,
            ..ctx_default()
        };
        let hints = workflow_hints(&ctx);
        assert!(hints.iter().any(|h| h.contains("committing")));
    }

    #[test]
    fn dedup_in_collect() {
        let actions = vec![ActionKind::Write, ActionKind::Write];
        let ctx = HintContext {
            intent: Intent::Ask,
            action_kinds: &[ActionKind::Write],
            step_num: 1,
            ..ctx_default()
        };
        let sources = default_sources();
        let hints = collect_hints(&ctx, &actions, |k| *k, &sources);
        let unique_count = hints.len();
        let mut deduped = hints.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(unique_count, deduped.len());
    }
}
