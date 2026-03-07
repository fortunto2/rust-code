//! Intent guard — data-driven action filtering by user intent.
//!
//! Architecture (Knuth-style, data over code):
//!
//! 1. `ActionKind` — coarse categories (enum, O(1) match)
//! 2. `action_kind()` — project-specific classifier (via SgrAgent trait method)
//! 3. `intent_allows()` — static permission matrix: intent × kind → allow/hint
//! 4. `StepDecision.hints` — soft nudges injected as HINT: system messages
//!
//! The guard does NOT block actions — it emits hints that steer the LLM.
//! Hard blocking would break tool-call contracts.

/// Coarse action categories. Agents map their specific tools to these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    /// Read-only: read file, search, list dir, git status/diff, project map
    Read,
    /// Write: write file, edit file, create file
    Write,
    /// Execute: bash, background tasks
    Execute,
    /// Git mutation: add, commit, push
    GitMutate,
    /// Plan/think: ask user, finish task, memory
    Plan,
    /// External: MCP tool calls, API calls
    External,
}

/// User intent — what mode the user selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    /// Full autonomy — all actions allowed.
    Auto,
    /// Ask before mutating — read/plan ok, write/execute get hints.
    Ask,
    /// Build mode — all actions allowed (same as Auto but explicit).
    Build,
    /// Plan only — no writes, no execution.
    Plan,
}

/// Result of intent check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentCheck {
    /// Action is allowed for this intent.
    Allow,
    /// Action is allowed but with a hint (soft nudge).
    Hint(String),
}

/// Static permission matrix: does this intent allow this action kind?
///
/// Returns `Allow` or `Hint(message)`. Never blocks.
///
/// ```text
///            Read  Write  Execute  GitMutate  Plan  External
/// Auto        ✓      ✓       ✓        ✓       ✓       ✓
/// Build       ✓      ✓       ✓        ✓       ✓       ✓
/// Ask         ✓    hint     hint     hint      ✓     hint
/// Plan        ✓    hint     hint     hint      ✓     hint
/// ```
pub fn intent_allows(intent: Intent, kind: ActionKind) -> IntentCheck {
    match (intent, kind) {
        // Auto and Build — full access
        (Intent::Auto | Intent::Build, _) => IntentCheck::Allow,

        // Ask/Plan — reads and planning always ok
        (_, ActionKind::Read | ActionKind::Plan) => IntentCheck::Allow,

        // Ask mode — hint on mutations
        (Intent::Ask, ActionKind::Write) => IntentCheck::Hint(
            "User is in ASK mode. Explain what you want to write and ask for confirmation before proceeding.".into(),
        ),
        (Intent::Ask, ActionKind::Execute) => IntentCheck::Hint(
            "User is in ASK mode. Describe the command you want to run and ask for confirmation.".into(),
        ),
        (Intent::Ask, ActionKind::GitMutate) => IntentCheck::Hint(
            "User is in ASK mode. Describe the git operation and ask for confirmation.".into(),
        ),
        (Intent::Ask, ActionKind::External) => IntentCheck::Hint(
            "User is in ASK mode. Describe the external call and ask for confirmation.".into(),
        ),

        // Plan mode — stronger hints on mutations
        (Intent::Plan, ActionKind::Write) => IntentCheck::Hint(
            "User is in PLAN mode. Do NOT write files. Instead describe what you would write and add it to the plan.".into(),
        ),
        (Intent::Plan, ActionKind::Execute) => IntentCheck::Hint(
            "User is in PLAN mode. Do NOT execute commands. Read-only commands (grep, find, cat) are ok for research.".into(),
        ),
        (Intent::Plan, ActionKind::GitMutate) => IntentCheck::Hint(
            "User is in PLAN mode. Do NOT make git changes. Add git operations to the plan instead.".into(),
        ),
        (Intent::Plan, ActionKind::External) => IntentCheck::Hint(
            "User is in PLAN mode. Do NOT make external calls. Add them to the plan instead.".into(),
        ),
    }
}

/// Check all actions in a step decision and collect hints.
///
/// Generic over action type — caller provides `classify` closure
/// that maps each action to an `ActionKind`.
pub fn guard_step<A>(
    intent: Intent,
    actions: &[A],
    classify: impl Fn(&A) -> ActionKind,
) -> Vec<String> {
    let mut hints = Vec::new();
    for action in actions {
        let kind = classify(action);
        if let IntentCheck::Hint(msg) = intent_allows(intent, kind) {
            // Deduplicate — same hint won't repeat for multiple similar actions
            if !hints.contains(&msg) {
                hints.push(msg);
            }
        }
    }
    hints
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_allows_everything() {
        for kind in [
            ActionKind::Read,
            ActionKind::Write,
            ActionKind::Execute,
            ActionKind::GitMutate,
            ActionKind::Plan,
            ActionKind::External,
        ] {
            assert_eq!(intent_allows(Intent::Auto, kind), IntentCheck::Allow);
        }
    }

    #[test]
    fn build_allows_everything() {
        for kind in [
            ActionKind::Read,
            ActionKind::Write,
            ActionKind::Execute,
            ActionKind::GitMutate,
            ActionKind::Plan,
            ActionKind::External,
        ] {
            assert_eq!(intent_allows(Intent::Build, kind), IntentCheck::Allow);
        }
    }

    #[test]
    fn ask_allows_reads() {
        assert_eq!(
            intent_allows(Intent::Ask, ActionKind::Read),
            IntentCheck::Allow
        );
        assert_eq!(
            intent_allows(Intent::Ask, ActionKind::Plan),
            IntentCheck::Allow
        );
    }

    #[test]
    fn ask_hints_on_writes() {
        assert!(matches!(
            intent_allows(Intent::Ask, ActionKind::Write),
            IntentCheck::Hint(_)
        ));
        assert!(matches!(
            intent_allows(Intent::Ask, ActionKind::Execute),
            IntentCheck::Hint(_)
        ));
    }

    #[test]
    fn plan_hints_on_mutations() {
        for kind in [
            ActionKind::Write,
            ActionKind::Execute,
            ActionKind::GitMutate,
            ActionKind::External,
        ] {
            assert!(
                matches!(intent_allows(Intent::Plan, kind), IntentCheck::Hint(_)),
                "Plan should hint on {:?}",
                kind
            );
        }
    }

    #[test]
    fn plan_allows_reads() {
        assert_eq!(
            intent_allows(Intent::Plan, ActionKind::Read),
            IntentCheck::Allow
        );
    }

    #[test]
    fn guard_step_deduplicates() {
        // Two writes should produce one hint, not two
        let actions = vec!["write_a", "write_b", "read_c"];
        let hints = guard_step(Intent::Ask, &actions, |a| {
            if a.starts_with("write") {
                ActionKind::Write
            } else {
                ActionKind::Read
            }
        });
        assert_eq!(hints.len(), 1);
    }

    #[test]
    fn guard_step_empty_on_auto() {
        let actions = vec!["write", "execute", "git"];
        let hints = guard_step(Intent::Auto, &actions, |_| ActionKind::Write);
        assert!(hints.is_empty());
    }

    #[test]
    fn guard_step_multiple_kinds() {
        let actions = vec!["write", "bash", "read"];
        let hints = guard_step(Intent::Plan, &actions, |a| match *a {
            "write" => ActionKind::Write,
            "bash" => ActionKind::Execute,
            _ => ActionKind::Read,
        });
        assert_eq!(hints.len(), 2); // one for write, one for execute
    }
}
