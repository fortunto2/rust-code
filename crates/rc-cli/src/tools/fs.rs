use anyhow::Result;
use std::path::Path;
use tokio::fs;

pub async fn read_file(path: impl AsRef<Path>, offset: Option<usize>, limit: Option<usize>) -> Result<String> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).await?;
    let lines: Vec<&str> = content.lines().collect();
    let total_lines = lines.len();
    
    let offset = offset.unwrap_or(0);
    let limit = limit.unwrap_or(1000);
    
    if offset >= total_lines {
        return Ok(format!("...[File has {} lines, requested offset {} is beyond end]...", total_lines, offset));
    }
    
    let end = (offset + limit).min(total_lines);
    let selected_lines = &lines[offset..end];
    let result = selected_lines.join("\n");
    
    let mut output = String::new();
    
    // Add header with pagination info
    if offset > 0 || end < total_lines {
        output.push_str(&format!("...[Lines {}-{} of {}]...\n\n", offset + 1, end, total_lines));
    }
    
    output.push_str(&result);
    
    // Add footer with pagination hint
    if end < total_lines {
        let next_offset = end;
        output.push_str(&format!("\n\n...[Use offset={} to read next {} lines]...", next_offset, limit));
    }
    
    Ok(output)
}

pub async fn write_file(path: impl AsRef<Path>, content: &str) -> Result<()> {
    let path = path.as_ref();
    // Create parent directories if they don't exist
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await?;
    }
    fs::write(path, content).await?;
    Ok(())
}

pub async fn edit_file(path: impl AsRef<Path>, old_string: &str, new_string: &str) -> Result<()> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).await?;
    
    let occurrences = content.matches(old_string).count();
    
    if occurrences == 0 {
        anyhow::bail!("Error: old_string not found in the file. Ensure the string matches exactly (including spaces/indentation).");
    } else if occurrences > 1 {
        anyhow::bail!("Error: old_string found {} times. Please provide a larger string block to ensure unique matching.", occurrences);
    }
    
    let updated_content = content.replacen(old_string, new_string, 1);
    fs::write(path, updated_content).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_existing_file() {
        let content = read_file("Cargo.toml", None, Some(5)).await.unwrap();
        assert!(content.contains("[workspace]") || content.contains("[package]"));
    }

    #[tokio::test]
    async fn read_nonexistent_file() {
        assert!(read_file("nonexistent_xyz.rs", None, None).await.is_err());
    }

    #[tokio::test]
    async fn write_and_read_roundtrip() {
        let path = "/tmp/rust-code-test-fs.txt";
        write_file(path, "test content").await.unwrap();
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("test content"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_file_replaces_string() {
        let path = "/tmp/rust-code-test-edit.txt";
        write_file(path, "hello world").await.unwrap();
        edit_file(path, "hello", "goodbye").await.unwrap();
        let content = read_file(path, None, None).await.unwrap();
        assert!(content.contains("goodbye world"));
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn edit_file_string_not_found() {
        let path = "/tmp/rust-code-test-edit2.txt";
        write_file(path, "hello").await.unwrap();
        let result = edit_file(path, "nonexistent", "replacement").await;
        assert!(result.is_err());
        std::fs::remove_file(path).ok();
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let path = "/tmp/rust-code-test-offset.txt";
        write_file(path, "line1\nline2\nline3\nline4\nline5").await.unwrap();
        let content = read_file(path, Some(1), Some(2)).await.unwrap();
        assert!(content.contains("line2"));
        assert!(content.contains("line3"));
        assert!(!content.contains("line1"));
        std::fs::remove_file(path).ok();
    }
}
