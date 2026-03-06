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

// Exec tool — run and wait for result
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

/// Run command in a named tmux window (non-blocking).
/// Returns immediately. Use `read_tmux_log` to check output.
pub async fn run_command_bg(name: &str, command: &str) -> Result<String> {
    // Ensure we're in a tmux session or create a detached one
    let session = "rc-bg";
    let safe_name = name.replace(' ', "-");

    // Try to create session if not exists, ignore error if it does
    let _ = Command::new("tmux")
        .args(["new-session", "-d", "-s", session])
        .output()
        .await;

    // Create a new window with the command
    let output = Command::new("tmux")
        .args([
            "new-window", "-t", session,
            "-n", &safe_name,
            "sh", "-c",
            &format!("{} 2>&1; echo '\\n[rc: done exit=$?]'; sleep 86400", command),
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-window failed: {}", stderr.trim());
    }

    Ok(format!("Started in tmux {}:{} — use F7 > Ctrl+O to attach", session, safe_name))
}

/// Read last N lines from a tmux window's buffer.
pub async fn read_tmux_log(name: &str, lines: usize) -> Result<String> {
    let session = "rc-bg";
    let target = format!("{}:{}", session, name.replace(' ', "-"));

    let output = Command::new("tmux")
        .args([
            "capture-pane", "-t", &target,
            "-p",           // print to stdout
            "-S", &format!("-{}", lines), // last N lines
        ])
        .output()
        .await?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux capture failed: {}", stderr.trim())
    }
}

/// List rc-bg tmux windows.
pub async fn list_bg_windows() -> Result<Vec<(String, String)>> {
    let output = Command::new("tmux")
        .args([
            "list-windows", "-t", "rc-bg",
            "-F", "#{window_name}|#{window_activity_flag}",
        ])
        .output()
        .await?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let txt = String::from_utf8_lossy(&output.stdout);
    Ok(txt.lines().filter_map(|l| {
        let parts: Vec<&str> = l.split('|').collect();
        if parts.len() >= 2 {
            Some((parts[0].to_string(), parts[1].to_string()))
        } else {
            None
        }
    }).collect())
}
