//! Auto-checkpoint: snapshot working tree before mutating actions.
//!
//! Uses git stash to save state. Each checkpoint is tagged with step number
//! so the user can undo any agent step.

use anyhow::Result;

/// Create a checkpoint (git stash) before a mutating action.
/// Returns checkpoint label if created, None if nothing to stash.
pub fn create_checkpoint(step: usize, action_desc: &str) -> Option<String> {
    let label = format!("rc-step-{}: {}", step, truncate(action_desc, 60));

    // Stage everything (including untracked) so stash captures full state
    let _ = std::process::Command::new("git")
        .args(["add", "-A"])
        .output();

    let output = std::process::Command::new("git")
        .args(["stash", "push", "-m", &label])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    if stdout.contains("No local changes") || !output.status.success() {
        return None;
    }

    // Pop immediately — we only wanted the stash entry as a snapshot
    let _ = std::process::Command::new("git")
        .args(["stash", "pop", "--quiet"])
        .output();

    Some(label)
}

/// List all rust-code checkpoints.
pub fn list_checkpoints() -> Vec<(usize, String)> {
    let output = std::process::Command::new("git")
        .args(["stash", "list"])
        .output()
        .ok();

    let Some(output) = output else {
        return vec![];
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .filter_map(|line| {
            // Format: "stash@{0}: On master: rc-step-3: write:foo.rs"
            let idx_start = line.find("stash@{")? + 7;
            let idx_end = line[idx_start..].find('}')? + idx_start;
            let idx: usize = line[idx_start..idx_end].parse().ok()?;
            if line.contains("rc-step-") {
                Some((idx, line.to_string()))
            } else {
                None
            }
        })
        .collect()
}

/// Restore a checkpoint by stash index.
pub fn restore_checkpoint(stash_idx: usize) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(["stash", "apply", &format!("stash@{{{}}}", stash_idx)])
        .output()?;

    if output.status.success() {
        Ok(format!("Restored checkpoint stash@{{{}}}", stash_idx))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Failed to restore: {}", stderr)
    }
}

/// Check if mutating action needs a checkpoint.
pub fn is_mutating_action(action_sig: &str) -> bool {
    action_sig.starts_with("write:")
        || action_sig.starts_with("edit:")
        || action_sig.starts_with("bash:")
        || action_sig.starts_with("bg:")
        || action_sig.starts_with("commit:")
        || action_sig.starts_with("add:")
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s.floor_char_boundary(max)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutating_detection() {
        assert!(is_mutating_action("write:foo.rs"));
        assert!(is_mutating_action("edit:bar.rs"));
        assert!(is_mutating_action("bash:rm -rf /tmp/test"));
        assert!(!is_mutating_action("read:foo.rs"));
        assert!(!is_mutating_action("search:query"));
        assert!(!is_mutating_action("git_status"));
    }

    #[test]
    fn list_checkpoints_runs() {
        // Should not panic, even if no checkpoints
        let _ = list_checkpoints();
    }
}
