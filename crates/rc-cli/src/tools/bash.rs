use anyhow::Result;
use tokio::process::Command;

fn truncate_output(output: String, max_len: usize) -> String {
    if output.len() > max_len {
        let truncated: String = output.chars().take(max_len).collect();
        format!(
            "{}\n\n...[Output truncated due to size. Total chars: {}]...",
            truncated,
            output.len()
        )
    } else {
        output
    }
}

// Exec tool — run and wait for result
pub async fn run_command(command: &str) -> Result<String> {
    let output = Command::new("sh").arg("-c").arg(command).output().await?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();

    if output.status.success() {
        Ok(truncate_output(stdout, 15000))
    } else {
        anyhow::bail!(
            "Command failed:\nstdout: {}\nstderr: {}",
            truncate_output(stdout, 5000),
            truncate_output(stderr, 5000)
        )
    }
}

const RC_SESSION: &str = "rc-bg";

/// Check if tmux is available.
async fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Ensure rc-bg session exists. Creates if needed.
async fn ensure_session() -> Result<()> {
    if !tmux_available().await {
        anyhow::bail!("tmux is not installed. Install it: brew install tmux");
    }
    // has-session returns 0 if exists
    let has = Command::new("tmux")
        .args(["has-session", "-t", RC_SESSION])
        .output()
        .await?;
    if !has.status.success() {
        let create = Command::new("tmux")
            .args(["new-session", "-d", "-s", RC_SESSION])
            .output()
            .await?;
        if !create.status.success() {
            let err = String::from_utf8_lossy(&create.stderr);
            anyhow::bail!(
                "Failed to create tmux session '{}': {}",
                RC_SESSION,
                err.trim()
            );
        }
    }
    Ok(())
}

/// Run command in a named tmux window (non-blocking).
pub async fn run_command_bg(name: &str, command: &str) -> Result<String> {
    ensure_session().await?;
    let safe_name = name.replace(' ', "-");

    // Kill existing window with same name to avoid duplicates
    let _ = Command::new("tmux")
        .args([
            "kill-window",
            "-t",
            &format!("{}:{}", RC_SESSION, safe_name),
        ])
        .output()
        .await;

    let output = Command::new("tmux")
        .args([
            "new-window",
            "-t",
            RC_SESSION,
            "-n",
            &safe_name,
            "sh",
            "-c",
            &format!(
                "{} 2>&1; echo ''; echo '[rc: exit='$?']'; read -r _dummy",
                command
            ),
        ])
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("tmux new-window failed: {}", stderr.trim());
    }

    Ok(format!(
        "Started in tmux {}:{} — F7 > Ctrl+O to attach",
        RC_SESSION, safe_name
    ))
}

/// Read last N lines from a tmux window's buffer.
pub async fn read_tmux_log(name: &str, lines: usize) -> Result<String> {
    let target = format!("{}:{}", RC_SESSION, name.replace(' ', "-"));

    let output = Command::new("tmux")
        .args([
            "capture-pane",
            "-t",
            &target,
            "-p",
            "-S",
            &format!("-{}", lines),
        ])
        .output()
        .await?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "Window '{}' not found or tmux error: {}",
            name,
            stderr.trim()
        )
    }
}

/// List rc-bg tmux windows with status.
pub async fn list_bg_windows() -> Result<Vec<(String, bool)>> {
    let output = Command::new("tmux")
        .args([
            "list-windows",
            "-t",
            RC_SESSION,
            "-F",
            "#{window_name}|#{pane_dead}",
        ])
        .output()
        .await;

    let output = match output {
        Ok(o) if o.status.success() => o,
        _ => return Ok(Vec::new()),
    };

    let txt = String::from_utf8_lossy(&output.stdout);
    Ok(txt
        .lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once('|')?;
            let done = rest == "1";
            Some((name.to_string(), done))
        })
        .collect())
}
