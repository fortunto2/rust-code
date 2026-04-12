//! Shared helpers for tool implementations.

use sgr_agent_core::agent_tool::ToolError;

/// Convert anyhow::Error to ToolError::Execution.
pub fn backend_err(e: anyhow::Error) -> ToolError {
    ToolError::Execution(e.to_string())
}

/// Default root path for tools that accept an optional root directory.
pub fn def_root() -> String {
    "/".into()
}

/// Default tree depth level.
pub fn def_level() -> i32 {
    2
}

/// Check if search output has actual matches (not just the header line).
pub fn has_matches(output: &str) -> bool {
    output.lines().any(|l| !l.starts_with('$') && !l.is_empty())
}

/// Parse search output for unique file paths (format: "path/file:line:content").
/// Returns up to `max` unique paths.
pub fn unique_files_from_search(output: &str, max: usize) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::new();
    for line in output.lines() {
        if line.starts_with('$') || line.is_empty() {
            continue;
        }
        if let Some(path) = line.split(':').next() {
            let path = path.trim();
            if !path.is_empty() && seen.insert(path.to_string()) {
                files.push(path.to_string());
                if files.len() > max {
                    return files;
                }
            }
        }
    }
    files
}

// ─── Output truncation (shared across all tool projects) ────────────────────

const MAX_OUTPUT_CHARS: usize = 30_000;
const PREFIX_LINES: usize = 200;
const SUFFIX_LINES: usize = 100;

/// Truncate tool output: keep first 200 + last 100 lines, omit middle.
///
/// Strategy from Codex: prefix has the most important info,
/// suffix has errors/summaries. Middle is expendable.
pub fn truncate_output(output: &str) -> String {
    if output.chars().count() <= MAX_OUTPUT_CHARS {
        return output.to_string();
    }
    let lines: Vec<&str> = output.lines().collect();
    let total = lines.len();
    if total <= PREFIX_LINES + SUFFIX_LINES {
        let prefix: String = output.chars().take(MAX_OUTPUT_CHARS / 2).collect();
        let suffix: String = output
            .chars()
            .rev()
            .take(MAX_OUTPUT_CHARS / 4)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return format!(
            "{}\n\n...[{} chars omitted]...\n\n{}",
            prefix,
            output.chars().count() - MAX_OUTPUT_CHARS / 2 - MAX_OUTPUT_CHARS / 4,
            suffix
        );
    }
    let prefix = &lines[..PREFIX_LINES];
    let suffix = &lines[total - SUFFIX_LINES..];
    let omitted = total - PREFIX_LINES - SUFFIX_LINES;
    format!(
        "Total output: {} lines\n\n{}\n\n...[{} lines omitted]...\n\n{}",
        total,
        prefix.join("\n"),
        omitted,
        suffix.join("\n"),
    )
}

#[cfg(test)]
mod tests {
    use super::truncate_output;
    use super::*;

    #[test]
    fn has_matches_empty() {
        assert!(!has_matches(""));
        assert!(!has_matches("$ rg pattern"));
        assert!(!has_matches("$ rg pattern\n"));
    }

    #[test]
    fn has_matches_with_results() {
        assert!(has_matches("$ rg pattern\nfile.txt:1:match"));
        assert!(has_matches("file.txt:1:match"));
    }

    #[test]
    fn unique_files_basic() {
        let output = "$ rg test\nfoo.txt:1:line1\nfoo.txt:2:line2\nbar.txt:1:line1";
        let files = unique_files_from_search(output, 10);
        assert_eq!(files, vec!["foo.txt", "bar.txt"]);
    }

    #[test]
    fn unique_files_respects_max() {
        let output = "a.txt:1:x\nb.txt:1:x\nc.txt:1:x";
        let files = unique_files_from_search(output, 2);
        assert_eq!(files.len(), 3); // max+1 for early exit detection
    }

    #[test]
    fn unique_files_skips_header() {
        let output = "$ rg test\n\nfile.txt:1:match";
        let files = unique_files_from_search(output, 10);
        assert_eq!(files, vec!["file.txt"]);
    }

    #[test]
    fn truncate_short_unchanged() {
        assert_eq!(truncate_output("hello\nworld"), "hello\nworld");
    }

    #[test]
    fn truncate_long_output() {
        let lines: Vec<String> = (0..500)
            .map(|i| format!("line {:04}: {}", i, "x".repeat(90)))
            .collect();
        let input = lines.join("\n");
        let result = truncate_output(&input);
        assert!(result.len() < input.len());
        assert!(result.contains("lines omitted"));
        assert!(result.contains("line 0000"));
        assert!(result.contains("line 0499"));
    }
}
