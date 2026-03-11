//! Tool output truncation — prefix+suffix strategy to keep context lean.
//!
//! Long outputs (bash, read_file, search) eat context window.
//! Instead of feeding full output, keep first + last lines with a summary.

/// Maximum output length in chars before truncation kicks in.
const MAX_OUTPUT_CHARS: usize = 30_000;

/// Lines to keep from the start of output.
const PREFIX_LINES: usize = 200;

/// Lines to keep from the end of output.
const SUFFIX_LINES: usize = 100;

/// Truncate tool output if it exceeds the threshold.
///
/// Strategy (from Codex):
/// - Keep first `PREFIX_LINES` lines (usually has the most important info)
/// - Keep last `SUFFIX_LINES` lines (errors, summaries at the end)
/// - Replace middle with a count of omitted lines
///
/// Returns the original string unchanged if under threshold.
pub fn truncate_output(output: &str) -> String {
    if output.chars().count() <= MAX_OUTPUT_CHARS {
        return output.to_string();
    }

    let lines: Vec<&str> = output.lines().collect();
    let total = lines.len();

    if total <= PREFIX_LINES + SUFFIX_LINES {
        // Few lines but very long lines — truncate by chars
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
    use super::*;

    #[test]
    fn short_output_unchanged() {
        let input = "hello\nworld";
        assert_eq!(truncate_output(input), input);
    }

    #[test]
    fn long_output_truncated() {
        // Generate 500 lines of 100 chars each = 50K chars
        let lines: Vec<String> = (0..500)
            .map(|i| format!("line {:04}: {}", i, "x".repeat(90)))
            .collect();
        let input = lines.join("\n");

        let result = truncate_output(&input);
        assert!(result.len() < input.len());
        assert!(result.contains("lines omitted"));
        assert!(result.contains("line 0000")); // prefix
        assert!(result.contains("line 0499")); // suffix
        assert!(!result.contains("line 0250")); // middle omitted
    }

    #[test]
    fn few_long_lines_truncated_by_chars() {
        // 10 lines of 5000 chars each
        let lines: Vec<String> = (0..10)
            .map(|i| format!("{}: {}", i, "y".repeat(5000)))
            .collect();
        let input = lines.join("\n");

        let result = truncate_output(&input);
        assert!(result.len() < input.len());
        assert!(result.contains("chars omitted"));
    }
}
