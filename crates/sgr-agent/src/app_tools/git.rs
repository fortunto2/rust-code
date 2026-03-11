//! Git tools: status, diff, add, commit.

use anyhow::{Context, Result};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct GitStatus {
    pub branch: String,
    pub dirty: bool,
    pub modified_files: Vec<String>,
    pub staged_files: Vec<String>,
    pub untracked_files: Vec<String>,
}

pub fn git_status() -> Result<Option<GitStatus>> {
    let check = Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !check {
        return Ok(None);
    }

    let branch_output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("Failed to get git branch")?;

    let branch = String::from_utf8_lossy(&branch_output.stdout)
        .trim()
        .to_string();

    let status_output = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .context("Failed to get git status")?;

    let status_str = String::from_utf8_lossy(&status_output.stdout);
    let mut modified = Vec::new();
    let mut staged = Vec::new();
    let mut untracked = Vec::new();

    for line in status_str.lines() {
        if let (Some(status), Some(file)) = (line.get(0..2), line.get(3..)) {
            let file = file.to_string();
            match status {
                " M" | "M " | "MM" => modified.push(file),
                "A " => staged.push(file),
                "??" => untracked.push(file),
                _ => {}
            }
        }
    }

    Ok(Some(GitStatus {
        branch,
        dirty: !status_str.trim().is_empty(),
        modified_files: modified,
        staged_files: staged,
        untracked_files: untracked,
    }))
}

pub fn git_diff(path: Option<&str>, cached: bool) -> Result<String> {
    let mut args = vec!["--no-pager", "diff"];

    if cached {
        args.push("--cached");
    }

    args.push("--no-color");

    if let Some(p) = path {
        args.push("--");
        args.push(p);
    }

    let output = Command::new("git")
        .args(&args)
        .env("GIT_PAGER", "cat")
        .output()
        .context("Failed to run git diff")?;

    if !output.status.success() {
        anyhow::bail!(
            "git diff failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn git_add(paths: &[String]) -> Result<()> {
    let mut args = vec!["add"];
    for path in paths {
        args.push(path);
    }

    let output = Command::new("git")
        .args(&args)
        .output()
        .context("Failed to run git add")?;

    if !output.status.success() {
        anyhow::bail!(
            "git add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

pub fn git_commit(message: &str) -> Result<()> {
    let output = Command::new("git")
        .args(["commit", "-m", message])
        .output()
        .context("Failed to run git commit")?;

    if !output.status.success() {
        anyhow::bail!(
            "git commit failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_status_in_repo() {
        let status = git_status().unwrap();
        assert!(status.is_some());
        let info = status.unwrap();
        assert!(!info.branch.is_empty());
    }

    #[test]
    fn git_diff_no_crash() {
        let _ = git_diff(None, false);
    }

    #[test]
    fn git_diff_specific_file() {
        let diff = git_diff(Some("Cargo.toml"), false);
        assert!(diff.is_ok());
    }
}
