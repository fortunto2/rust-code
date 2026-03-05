use anyhow::Result;
use tokio::process::Command;

pub async fn run_command(command: &str) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        anyhow::bail!("Command failed:\nstdout: {}\nstderr: {}", stdout, stderr)
    }
}
