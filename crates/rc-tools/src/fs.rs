use anyhow::Result;
use std::path::Path;
use tokio::fs;

pub async fn read_file(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).await?;
    
    // Truncate to max 1000 lines or 30000 chars to avoid blowing up context
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() > 1000 {
        let truncated = lines[..1000].join("\n");
        return Ok(format!("{}\n\n...[File truncated. Total lines: {}]...", truncated, lines.len()));
    }
    
    if content.len() > 30000 {
        let truncated: String = content.chars().take(30000).collect();
        return Ok(format!("{}\n\n...[File truncated due to size. Total chars: {}]...", truncated, content.len()));
    }
    
    Ok(content)
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
