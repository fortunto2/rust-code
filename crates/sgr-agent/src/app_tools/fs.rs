//! Filesystem tools: read, write, edit with pagination and safe replacements.

use anyhow::Result;
use std::path::Path;
use tokio::fs;

pub async fn read_file(
    path: impl AsRef<Path>,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).await?;
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();

    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(1000);

    if offset >= total_lines {
        return Ok(format!(
            "...[File has {} lines, requested offset {} is beyond end]...",
            total_lines, offset
        ));
    }

    let end = (offset + limit).min(total_lines);
    let selected_lines = &lines[offset..end];
    let result = selected_lines.join("\n");

    let mut output = String::new();

    if offset > 0 || end < total_lines {
        output.push_str(&format!(
            "...[Lines {}-{} of {}]...\n\n",
            offset + 1,
            end,
            total_lines
        ));
    }

    output.push_str(&result);

    if end < total_lines {
        let next_offset = end;
        output.push_str(&format!(
            "\n\n...[Use offset={} to read next {} lines]...",
            next_offset, limit
        ));
    }

    Ok(output)
}

pub async fn write_file(path: impl AsRef<Path>, content: &str) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, content).await?;
    Ok(())
}

pub async fn edit_file(path: impl AsRef<Path>, old_string: &str, new_string: &str) -> Result<()> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).await?;

    // Idempotency: if new_string is already in the file and old_string is not,
    // the edit was already applied — return success without modifying.
    if !content.contains(old_string) && content.contains(new_string) {
        return Ok(());
    }

    // Try exact match first
    if let Some(pos) = content.find(old_string) {
        if content[pos + old_string.len()..].contains(old_string) {
            anyhow::bail!(
                "Error: old_string found multiple times. Please provide a larger string block to ensure unique matching."
            );
        }
        return apply_edit(path, &content, pos, old_string.len(), new_string).await;
    }

    // Fallback: whitespace-normalized match (tabs↔spaces, trailing whitespace)
    if let Some((pos, actual_len)) = find_ws_normalized(&content, old_string) {
        let _actual_match = &content[pos..pos + actual_len];
        if find_ws_normalized(&content[pos + actual_len..], old_string).is_some() {
            anyhow::bail!(
                "Error: old_string found multiple times (whitespace-normalized). Provide more context."
            );
        }
        eprintln!(
            "[edit_file] Whitespace-normalized match used for {}",
            path.display()
        );
        return apply_edit(path, &content, pos, actual_len, new_string).await;
    }

    // Diagnostics: find closest matching lines to help the agent
    let diagnostic = find_closest_lines(&content, old_string);
    anyhow::bail!(
        "Error: old_string not found in file (even with whitespace normalization).{}\n\
         Hint: use read_file to see exact content, then copy the exact lines into old_string.",
        diagnostic
    );
}

async fn apply_edit(
    path: &Path,
    content: &str,
    pos: usize,
    old_len: usize,
    new_string: &str,
) -> Result<()> {
    let mut updated = String::with_capacity(content.len() - old_len + new_string.len());
    updated.push_str(&content[..pos]);
    updated.push_str(new_string);
    updated.push_str(&content[pos + old_len..]);
    fs::write(path, updated).await?;
    Ok(())
}

/// Normalize whitespace for comparison: collapse runs of spaces/tabs into single space,
/// trim trailing whitespace per line.
fn normalize_ws(s: &str) -> String {
    s.lines()
        .map(|line| {
            let mut result = String::new();
            let mut in_ws = false;
            for ch in line.chars() {
                if ch == ' ' || ch == '\t' {
                    if !in_ws {
                        result.push(' ');
                        in_ws = true;
                    }
                } else {
                    result.push(ch);
                    in_ws = false;
                }
            }
            // Trim trailing space from normalization
            result.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Find old_string in content using whitespace-normalized comparison.
/// Returns (byte_pos, byte_len) of the actual match in `content`.
fn find_ws_normalized(content: &str, old_string: &str) -> Option<(usize, usize)> {
    let old_lines: Vec<&str> = old_string.lines().collect();
    if old_lines.is_empty() {
        return None;
    }
    let norm_old: Vec<String> = old_lines.iter().map(|l| normalize_ws(l)).collect();

    let content_lines: Vec<&str> = content.lines().collect();
    if content_lines.len() < old_lines.len() {
        return None;
    }

    for start in 0..=content_lines.len() - old_lines.len() {
        let mut matched = true;
        for (i, norm) in norm_old.iter().enumerate() {
            if normalize_ws(content_lines[start + i]) != *norm {
                matched = false;
                break;
            }
        }
        if matched {
            // Calculate byte positions
            let byte_start: usize = content_lines[..start]
                .iter()
                .map(|l| l.len() + 1) // +1 for \n
                .sum();
            let byte_end: usize = byte_start
                + content_lines[start..start + old_lines.len()]
                    .iter()
                    .enumerate()
                    .map(|(i, l)| l.len() + if i < old_lines.len() - 1 { 1 } else { 0 })
                    .sum::<usize>();
            // Verify byte_end doesn't exceed content
            if byte_end <= content.len() {
                return Some((byte_start, byte_end - byte_start));
            }
        }
    }
    None
}

/// Find the closest matching lines in content to help diagnose the mismatch.
fn find_closest_lines(content: &str, old_string: &str) -> String {
    let old_first = old_string.lines().next().unwrap_or("").trim();
    if old_first.is_empty() {
        return String::new();
    }

    // Find lines in the file that contain the first line's content (trimmed)
    let mut matches: Vec<(usize, &str)> = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if line.trim().contains(old_first) || old_first.contains(line.trim()) {
            matches.push((i + 1, line));
        }
    }

    if matches.is_empty() {
        return format!(
            "\nFirst line of old_string: {:?}\nNo similar line found in file.",
            truncate(old_first, 80)
        );
    }

    let mut diag = format!(
        "\nFirst line of old_string: {:?}\nSimilar lines in file:",
        truncate(old_first, 80)
    );
    for (line_no, line) in matches.iter().take(3) {
        diag.push_str(&format!("\n  L{}: {:?}", line_no, truncate(line, 80)));
    }
    diag
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn write_and_read_roundtrip() {
        let path = "/tmp/baml-agent-test-fs.txt";
        write_file(path, "test content").await.unwrap();
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("test content"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_replaces_string() {
        let path = "/tmp/baml-agent-test-edit.txt";
        write_file(path, "hello world").await.unwrap();
        edit_file(path, "hello", "goodbye").await.unwrap();
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("goodbye world"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_not_found_errors() {
        let path = "/tmp/baml-agent-test-edit2.txt";
        write_file(path, "hello").await.unwrap();
        let result = edit_file(path, "nonexistent", "replacement").await;
        assert!(result.is_err());
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let path = "/tmp/baml-agent-test-offset.txt";
        write_file(path, "line1\nline2\nline3\nline4\nline5")
            .await
            .unwrap();
        let content = read_file(path, Some(1), Some(2)).await.unwrap();
        assert!(content.contains("line2"));
        assert!(content.contains("line3"));
        assert!(!content.contains("line1"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_multiple_occurrences_errors() {
        let path = "/tmp/baml-agent-test-multi.txt";
        write_file(path, "foo bar foo").await.unwrap();
        let result = edit_file(path, "foo", "baz").await;
        assert!(result.is_err());
        // File should be unchanged
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("foo bar foo"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn read_nonexistent_errors() {
        assert!(read_file("nonexistent_xyz.rs", None, None).await.is_err());
    }

    #[tokio::test]
    async fn edit_ws_normalized_tabs_vs_spaces() {
        let path = "/tmp/baml-agent-test-ws1.txt";
        // File has tabs
        write_file(path, "fn main() {\n\tprintln!(\"hello\");\n}\n")
            .await
            .unwrap();
        // Agent sends spaces
        edit_file(path, "    println!(\"hello\");", "    println!(\"world\");")
            .await
            .unwrap();
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("world"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_ws_normalized_trailing_spaces() {
        let path = "/tmp/baml-agent-test-ws2.txt";
        // File has trailing spaces
        write_file(path, "hello world  \nfoo bar\n").await.unwrap();
        // Agent sends without trailing spaces
        edit_file(path, "hello world\nfoo bar", "goodbye world\nfoo baz")
            .await
            .unwrap();
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("goodbye world"));
        assert!(content.contains("foo baz"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_ws_normalized_multiple_spaces() {
        let path = "/tmp/baml-agent-test-ws3.txt";
        // File has 4 spaces indent
        write_file(path, "if true {\n    let x = 1;\n}\n")
            .await
            .unwrap();
        // Agent sends 2 spaces indent
        edit_file(path, "  let x = 1;", "  let x = 2;")
            .await
            .unwrap();
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("let x = 2"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_diagnostic_shows_similar_lines() {
        let path = "/tmp/baml-agent-test-diag.txt";
        write_file(path, "fn main() {\n    println!(\"hello\");\n}\n")
            .await
            .unwrap();
        // Completely wrong old_string but first line matches
        let result = edit_file(path, "fn main() {\n    WRONG_LINE\n}", "replacement").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Similar lines") || err.contains("First line"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_idempotent_already_applied() {
        let path = "/tmp/baml-agent-test-idempotent.txt";
        write_file(path, "fn main() {\n    let x = 2;\n}\n")
            .await
            .unwrap();
        // Try to apply an edit that's already done (old_string absent, new_string present)
        let result = edit_file(path, "let x = 1;", "let x = 2;").await;
        assert!(result.is_ok()); // Should succeed silently
        // File should be unchanged
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("let x = 2"));
        assert!(!content.contains("let x = 1"));
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn normalize_ws_basic() {
        assert_eq!(normalize_ws("\t\thello  world  "), " hello world");
        assert_eq!(normalize_ws("  a  b  "), " a b");
        assert_eq!(normalize_ws("no_change"), "no_change");
    }

    #[test]
    fn find_ws_normalized_basic() {
        let content = "fn main() {\n\tlet x = 1;\n\tlet y = 2;\n}\n";
        let old = "  let x = 1;\n  let y = 2;";
        let result = find_ws_normalized(content, old);
        assert!(result.is_some());
        let (pos, len) = result.unwrap();
        assert_eq!(&content[pos..pos + len], "\tlet x = 1;\n\tlet y = 2;");
    }
}
