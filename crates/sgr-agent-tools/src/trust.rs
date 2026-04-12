//! Trust metadata — tag file reads with trust level for LLM safety.
//!
//! Only root-level system files (AGENTS.md, README.md) are trusted.
//! Everything else is untrusted (may contain prompt injection).

/// Infer trust level from file path.
///
/// Root-level AGENTS.md and README.md are trusted (workspace policy).
/// All other files are untrusted.
pub fn infer_trust(path: &str) -> &'static str {
    let normalized = path.trim_start_matches('/');
    let parts: Vec<&str> = normalized.split('/').collect();
    if parts.len() == 1 {
        let lower = parts[0].to_lowercase();
        if lower == "agents.md" || lower == "readme.md" {
            return "trusted";
        }
    }
    "untrusted"
}

/// Wrap content with trust metadata header: `[path | trust_level]`
pub fn wrap_with_meta(path: &str, content: &str) -> String {
    let trust = infer_trust(path);
    format!("[{} | {}]\n{}", path, trust, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_agents_md_trusted() {
        assert_eq!(infer_trust("AGENTS.MD"), "trusted");
        assert_eq!(infer_trust("agents.md"), "trusted");
        assert_eq!(infer_trust("/AGENTS.MD"), "trusted");
    }

    #[test]
    fn root_readme_trusted() {
        assert_eq!(infer_trust("README.MD"), "trusted");
        assert_eq!(infer_trust("readme.md"), "trusted");
    }

    #[test]
    fn nested_files_untrusted() {
        assert_eq!(infer_trust("contacts/agents.md"), "untrusted");
        assert_eq!(infer_trust("/docs/readme.md"), "untrusted");
        assert_eq!(infer_trust("inbox/msg.txt"), "untrusted");
    }

    #[test]
    fn wrap_with_meta_format() {
        let result = wrap_with_meta("AGENTS.MD", "content");
        assert_eq!(result, "[AGENTS.MD | trusted]\ncontent");

        let result = wrap_with_meta("inbox/msg.txt", "hello");
        assert_eq!(result, "[inbox/msg.txt | untrusted]\nhello");
    }
}
