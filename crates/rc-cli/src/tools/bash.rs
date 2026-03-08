use anyhow::Result;
use std::path::{Path, PathBuf};
use tokio::process::Command;

fn truncate_output(output: String, max_bytes: usize) -> String {
    if output.len() > max_bytes {
        // Find a valid char boundary at or before max_bytes
        let mut end = max_bytes;
        while end > 0 && !output.is_char_boundary(end) {
            end -= 1;
        }
        format!(
            "{}\n\n...[Output truncated. Total bytes: {}]...",
            &output[..end],
            output.len()
        )
    } else {
        output
    }
}

// Exec tool — run and wait for result (legacy, used by SearchCodeTool)
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

/// Default timeout: 2 minutes.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Max timeout: 10 minutes.
const MAX_TIMEOUT_MS: u64 = 600_000;

/// Run a command with persistent CWD and optional timeout.
/// Always returns output + exit code — never errors on non-zero exit.
/// CWD is tracked: if the command contains `cd`, the new CWD is returned.
pub async fn run_command_in(command: &str, cwd: &Path, timeout_ms: Option<u64>) -> BashResult {
    let timeout = std::time::Duration::from_millis(
        timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).min(MAX_TIMEOUT_MS),
    );

    let result = tokio::time::timeout(timeout, run_interactive(command, cwd)).await;

    match result {
        Ok(bash_result) => bash_result,
        Err(_) => BashResult {
            output: format!(
                "Command timed out after {}s. Consider using bash_bg for long-running commands.",
                timeout.as_secs()
            ),
            exit_code: 124, // standard timeout exit code
            cwd: cwd.to_path_buf(),
        },
    }
}

/// Result of an interactive bash command (for TUI bash mode).
/// Always returns output — never errors on non-zero exit.
pub struct BashResult {
    pub output: String,
    pub exit_code: i32,
    /// New CWD after command (tracks `cd`).
    pub cwd: PathBuf,
}

/// Run a command in interactive bash mode with persistent CWD.
/// Unlike `run_command`, this:
/// - Tracks CWD across invocations
/// - Shows both stdout and stderr
/// - Never errors on non-zero exit code
pub async fn run_interactive(command: &str, cwd: &Path) -> BashResult {
    // Wrap command to capture CWD after execution
    let wrapped = format!("{command}\n__rc_exit=$?\necho __RC_CWD_MARKER__\npwd\nexit $__rc_exit");
    let result = Command::new("bash")
        .arg("-c")
        .arg(&wrapped)
        .current_dir(cwd)
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env(
            "TERM",
            std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into()),
        )
        .output()
        .await;

    match result {
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            // Extract CWD from stdout marker
            let (user_stdout, new_cwd) = if let Some(pos) = stdout.rfind("__RC_CWD_MARKER__\n") {
                let user_part = &stdout[..pos];
                let cwd_part = stdout[pos + "__RC_CWD_MARKER__\n".len()..].trim();
                (user_part.to_string(), PathBuf::from(cwd_part))
            } else {
                (stdout.to_string(), cwd.to_path_buf())
            };

            // Combine stdout + stderr like a real terminal
            let mut combined = String::new();
            let trimmed_out = user_stdout.trim_end();
            let trimmed_err = stderr.trim_end();
            if !trimmed_out.is_empty() {
                combined.push_str(trimmed_out);
            }
            if !trimmed_err.is_empty() {
                if !combined.is_empty() {
                    combined.push('\n');
                }
                combined.push_str(trimmed_err);
            }

            BashResult {
                output: truncate_output(combined, 15000),
                exit_code,
                cwd: new_cwd,
            }
        }
        Err(e) => BashResult {
            output: format!("Failed to spawn bash: {e}"),
            exit_code: -1,
            cwd: cwd.to_path_buf(),
        },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn interactive_echo() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_interactive("echo hello", &cwd).await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.output.trim(), "hello");
        assert_eq!(r.cwd, cwd);
    }

    #[tokio::test]
    async fn interactive_cd_changes_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_interactive("cd /tmp", &cwd).await;
        assert_eq!(r.exit_code, 0);
        // CWD should be /tmp (or /private/tmp on macOS)
        assert!(r.cwd.ends_with("tmp"), "expected /tmp, got {:?}", r.cwd);
    }

    #[tokio::test]
    async fn interactive_nonzero_exit() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_interactive("false", &cwd).await;
        assert_eq!(r.exit_code, 1);
    }

    #[tokio::test]
    async fn run_command_in_timeout() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_command_in("sleep 10", &cwd, Some(500)).await;
        assert_eq!(r.exit_code, 124);
        assert!(r.output.contains("timed out"), "got: {}", r.output);
    }

    #[tokio::test]
    async fn run_command_in_preserves_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_command_in("cd /tmp && echo ok", &cwd, None).await;
        assert_eq!(r.exit_code, 0);
        assert!(r.cwd.ends_with("tmp"), "cwd: {:?}", r.cwd);
        assert!(r.output.contains("ok"));
    }

    #[tokio::test]
    async fn run_command_in_nonzero_no_error() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_command_in("exit 42", &cwd, None).await;
        assert_eq!(r.exit_code, 42);
    }

    #[tokio::test]
    async fn interactive_stderr_shown() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_interactive("echo err >&2", &cwd).await;
        assert_eq!(r.exit_code, 0);
        assert!(r.output.contains("err"), "stderr not shown: {}", r.output);
    }
}
