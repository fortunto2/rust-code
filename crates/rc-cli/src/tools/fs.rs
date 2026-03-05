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
