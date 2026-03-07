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

You operate in an SGR (Schema-Guided Reasoning) loop:
1. Analyze the current state and conversation history
2. Decide the next action(s) using the structured output format
3. Receive tool results
4. Repeat until the task is complete

## Rules
- Always set `task_completed = true` when the goal is achieved
- If you are stuck or repeating actions, try a different approach
- Break complex tasks into small sequential steps
- Report errors clearly — do not silently retry the same failing action
- {domain_rules}

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
    fn base_prompt_has_sgr_structure() {
        assert!(BASE_SYSTEM_PROMPT.contains("SGR"));
        assert!(BASE_SYSTEM_PROMPT.contains("task_completed"));
        assert!(BASE_SYSTEM_PROMPT.contains("{role}"));
        assert!(BASE_SYSTEM_PROMPT.contains("{tools_reference}"));
        assert!(BASE_SYSTEM_PROMPT.contains("{domain_rules}"));
        assert!(BASE_SYSTEM_PROMPT.contains("{output_format}"));
    }
}
