use anyhow::Result;
use std::path::Path;
use tokio::fs;

pub async fn read_file(path: impl AsRef<Path>) -> Result<String> {
    let path = path.as_ref();
    let content = fs::read_to_string(path).await?;
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
