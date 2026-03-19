//! Model metadata — context window sizes and compaction budgets.
//!
//! Maps model name → context window. Used by any agent doing compaction
//! to determine when summarization is needed.

/// Estimate context window size for a model by name.
///
/// Recognizes major model families: Gemini, GPT-5, Claude, DeepSeek.
/// Returns token count. Falls back to 128K for unknown models.
pub fn context_window(model_id: &str) -> usize {
    let id = model_id.to_lowercase();
    if id.contains("gemini") {
        1_000_000
    } else if id.contains("gpt-5") && id.contains("mini") {
        400_000
    } else if id.contains("gpt-5") {
        1_050_000
    } else if id.contains("claude") {
        200_000
    } else if id.contains("deepseek") {
        128_000
    } else {
        // GPT-4o, Mistral, Llama, etc.
        128_000
    }
}

/// Suggested compaction threshold (60% of context window).
///
/// Delays compaction as long as possible since it costs an LLM call.
/// For a 400K model this means ~240K tokens.
pub fn compaction_budget(model_id: &str) -> usize {
    context_window(model_id) * 6 / 10
}

/// Strip provider namespace: `"openai/gpt-5.4"` → `"gpt-5.4"`.
///
/// Handles common prefixes: `openai/`, `google/`, `anthropic/`.
pub fn strip_provider(model_id: &str) -> &str {
    for prefix in ["openai/", "google/", "anthropic/"] {
        if let Some(stripped) = model_id.strip_prefix(prefix) {
            return stripped;
        }
    }
    model_id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_context() {
        assert_eq!(context_window("gemini-2.5-flash"), 1_000_000);
        assert_eq!(context_window("gemini-2.5-pro"), 1_000_000);
    }

    #[test]
    fn gpt5_context() {
        assert_eq!(context_window("gpt-5.4-mini"), 400_000);
        assert_eq!(context_window("gpt-5.4"), 1_050_000);
        assert_eq!(context_window("gpt-5.4-pro"), 1_050_000);
    }

    #[test]
    fn claude_context() {
        assert_eq!(context_window("claude-sonnet-4-5"), 200_000);
        assert_eq!(context_window("claude-opus-4-6"), 200_000);
    }

    #[test]
    fn deepseek_context() {
        assert_eq!(context_window("deepseek-chat"), 128_000);
    }

    #[test]
    fn fallback_context() {
        assert_eq!(context_window("gpt-4o"), 128_000);
        assert_eq!(context_window("unknown-model"), 128_000);
    }

    #[test]
    fn budget_is_60_percent() {
        assert_eq!(compaction_budget("gemini-2.5-flash"), 600_000);
        assert_eq!(compaction_budget("gpt-5.4-mini"), 240_000);
        assert_eq!(compaction_budget("claude-opus-4-6"), 120_000);
        assert_eq!(compaction_budget("gpt-4o"), 76_800);
    }

    #[test]
    fn strip_provider_openai() {
        assert_eq!(strip_provider("openai/gpt-5.4-mini"), "gpt-5.4-mini");
        assert_eq!(strip_provider("openai/gpt-5.4"), "gpt-5.4");
    }

    #[test]
    fn strip_provider_google() {
        assert_eq!(
            strip_provider("google/gemini-2.5-flash"),
            "gemini-2.5-flash"
        );
    }

    #[test]
    fn strip_provider_anthropic() {
        assert_eq!(
            strip_provider("anthropic/claude-opus-4-6"),
            "claude-opus-4-6"
        );
    }

    #[test]
    fn strip_provider_noop() {
        assert_eq!(strip_provider("gpt-5.4-mini"), "gpt-5.4-mini");
        assert_eq!(strip_provider("claude-opus-4-6"), "claude-opus-4-6");
    }
}
