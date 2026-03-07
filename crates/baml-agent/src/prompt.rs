/// Base system prompt template for SGR agents.
///
/// This is a starting point — each project should customize it
/// for their domain (video montage, sales, code review, etc.).
///
/// Placeholders:
/// - `{role}` — agent persona (e.g. "video montage director")
/// - `{tools_reference}` — list of available tools/tasks
/// - `{domain_rules}` — domain-specific constraints
/// - `{output_format}` — BAML injects this via `{{ ctx.output_format }}`
///
/// In BAML, use `{{ ctx.output_format }}` instead of `{output_format}`.
pub const BASE_SYSTEM_PROMPT: &str = r#"You are a {role}.

You operate in a STAR (Situation-Task-Action-Result) loop:

**S — Situation**: Assess current state. What phase are we in? What's done, what's blocking?
Scan the ENTIRE conversation history — never re-ask what's already known.

**T — Task**: List 1-5 remaining steps. First item = what you're doing RIGHT NOW.
Must always advance toward completion — never idle or repeat.

**A — Action**: Execute the first task step using available tools.
Use multiple actions for independent operations, single when steps depend on each other.

**R — Result**: Set task_completed = true only when ALL steps are done.
Verify before finishing. Report errors clearly.

## Rules
- {domain_rules}
- If a tool returns "not found", "no matches", or empty output — that IS the answer. Accept it. Do NOT retry with different flags, quotes, or syntax.
- Never run the same command more than once. If the result was empty, it will stay empty.
- After each tool result, always ADVANCE to the next step. Never retry the same operation.
- If stuck, try a fundamentally different approach or report to the user.

## Available Tools
{tools_reference}

## Output Format
Respond with a JSON object matching the schema below.
{output_format}
"#;

/// Build a system prompt from the base template.
///
/// # Example
/// ```
/// use baml_agent::prompt::build_system_prompt;
///
/// let prompt = build_system_prompt(
///     "video montage director",
///     "- analyze: analyze video segments\n- assemble: build timeline",
///     "Prefer chronological order. Use beat-sync when music is provided.",
/// );
/// assert!(prompt.contains("video montage director"));
/// assert!(prompt.contains("analyze video segments"));
/// ```
pub fn build_system_prompt(role: &str, tools_reference: &str, domain_rules: &str) -> String {
    BASE_SYSTEM_PROMPT
        .replace("{role}", role)
        .replace("{tools_reference}", tools_reference)
        .replace("{domain_rules}", domain_rules)
        // Leave {output_format} as-is — BAML replaces it with {{ ctx.output_format }}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_prompt_replaces_placeholders() {
        let prompt = build_system_prompt(
            "sales assistant",
            "- search_crm: find contacts\n- send_email: compose and send",
            "Always be polite. Never share internal pricing.",
        );
        assert!(prompt.contains("You are a sales assistant."));
        assert!(prompt.contains("search_crm: find contacts"));
        assert!(prompt.contains("Always be polite"));
        // output_format placeholder preserved for BAML
        assert!(prompt.contains("{output_format}"));
    }

    #[test]
    fn base_prompt_has_star_structure() {
        assert!(BASE_SYSTEM_PROMPT.contains("STAR"));
        assert!(BASE_SYSTEM_PROMPT.contains("Situation"));
        assert!(BASE_SYSTEM_PROMPT.contains("task_completed"));
        assert!(BASE_SYSTEM_PROMPT.contains("{role}"));
        assert!(BASE_SYSTEM_PROMPT.contains("{tools_reference}"));
        assert!(BASE_SYSTEM_PROMPT.contains("{domain_rules}"));
        assert!(BASE_SYSTEM_PROMPT.contains("{output_format}"));
    }
}
