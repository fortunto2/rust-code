//! Bash command execution with persistent CWD and timeout.

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

/// Legacy blocking run — errors on non-zero exit.
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

/// Result of a bash command — always succeeds (no Result error on non-zero exit).
pub struct BashResult {
    pub output: String,
    pub exit_code: i32,
    /// New CWD after command (tracks `cd`).
    pub cwd: PathBuf,
}

/// Default timeout: 2 minutes.
const DEFAULT_TIMEOUT_MS: u64 = 120_000;
/// Max timeout: 10 minutes.
const MAX_TIMEOUT_MS: u64 = 600_000;

/// Run a command with persistent CWD and optional timeout.
/// Always returns output + exit code — never errors on non-zero exit.
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
            exit_code: 124,
            cwd: cwd.to_path_buf(),
        },
    }
}

/// Run a command with CWD tracking.
/// Shows both stdout and stderr, never errors on non-zero exit code.
pub async fn run_interactive(command: &str, cwd: &Path) -> BashResult {
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

            let (user_stdout, new_cwd) = if let Some(pos) = stdout.rfind("__RC_CWD_MARKER__\n") {
                let user_part = &stdout[..pos];
                let cwd_part = stdout[pos + "__RC_CWD_MARKER__\n".len()..].trim();
                (user_part.to_string(), PathBuf::from(cwd_part))
            } else {
                (stdout.to_string(), cwd.to_path_buf())
            };

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn echo_works() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_interactive("echo hello", &cwd).await;
        assert_eq!(r.exit_code, 0);
        assert_eq!(r.output.trim(), "hello");
    }

    #[tokio::test]
    async fn cd_changes_cwd() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_command_in("cd /tmp && echo ok", &cwd, None).await;
        assert_eq!(r.exit_code, 0);
        assert!(r.cwd.ends_with("tmp"));
        assert!(r.output.contains("ok"));
    }

    #[tokio::test]
    async fn nonzero_exit_no_error() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_command_in("exit 42", &cwd, None).await;
        assert_eq!(r.exit_code, 42);
    }

    #[tokio::test]
    async fn timeout_returns_124() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_command_in("sleep 10", &cwd, Some(500)).await;
        assert_eq!(r.exit_code, 124);
        assert!(r.output.contains("timed out"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        // "Привет" = 12 bytes (2 bytes per char), truncate at 7 should not split a char
        let s = "Привет".to_string(); // 12 bytes
        let result = truncate_output(s, 7);
        // Should cut at byte 6 (3 chars "При"), not panic or produce invalid UTF-8
        assert!(result.contains("При"));
        assert!(!result.contains("в")); // 4th char starts at byte 6, cut before byte 7
        assert!(result.contains("truncated"));
    }

    #[tokio::test]
    async fn stderr_shown() {
        let cwd = std::env::current_dir().unwrap();
        let r = run_interactive("echo err >&2", &cwd).await;
        assert_eq!(r.exit_code, 0);
        assert!(r.output.contains("err"));
    }
}
