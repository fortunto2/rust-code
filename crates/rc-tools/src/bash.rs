use anyhow::Result;
use tokio::process::Command;

fn truncate_output(output: String, max_len: usize) -> String {
    if output.len() > max_len {
        let truncated: String = output.chars().take(max_len).collect();
        format!("{}\n\n...[Output truncated due to size. Total chars: {}]...", truncated, output.len())
    } else {
        output
    }
}

// Exec tool
pub async fn run_command(command: &str) -> Result<String> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .output()
        .await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(truncate_output(stdout, 15000))
    } else {
        anyhow::bail!("Command failed:\nstdout: {}\nstderr: {}", truncate_output(stdout, 5000), truncate_output(stderr, 5000))
    }
}
