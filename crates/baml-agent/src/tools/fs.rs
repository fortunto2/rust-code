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

    // Single-pass: find first occurrence, then check for a second
    let Some(pos) = content.find(old_string) else {
        anyhow::bail!(
            "Error: old_string not found in the file. Ensure the string matches exactly (including spaces/indentation). Try using read_file first to see the current file contents before editing."
        );
    };

    if content[pos + old_string.len()..].contains(old_string) {
        anyhow::bail!(
            "Error: old_string found multiple times. Please provide a larger string block to ensure unique matching."
        );
    }

    let mut updated = String::with_capacity(content.len() - old_string.len() + new_string.len());
    updated.push_str(&content[..pos]);
    updated.push_str(new_string);
    updated.push_str(&content[pos + old_string.len()..]);
    fs::write(path, updated).await?;
    Ok(())
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
}
