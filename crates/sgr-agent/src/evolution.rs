//! Self-evolution: agent evaluates its own runs and proposes improvements.
//!
//! After each task, the agent can analyze telemetry and identify bottlenecks.
//! Improvements are stored as tasks in `.tasks/` for the next run.
//!
//! ## Evolution loop
//! ```text
//! Run task → Collect telemetry → Evaluate → Propose improvements →
//! → Pick improvement → Patch code → Test → Commit → Rebuild → Restart →
//! → Run task (with improvement) → ...
//! ```

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Telemetry from a single agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunStats {
    /// Total steps taken
    pub steps: usize,
    /// Tool errors encountered
    pub tool_errors: usize,
    /// Loop warnings triggered
    pub loop_warnings: usize,
    /// Loop aborts triggered
    pub loop_aborts: usize,
    /// apply_patch failures
    pub patch_failures: usize,
    /// Successful tool calls
    pub successful_calls: usize,
    /// Task completed (vs aborted)
    pub completed: bool,
    /// Cost estimate (characters in/out)
    pub cost_chars: usize,
}

impl Default for RunStats {
    fn default() -> Self {
        Self {
            steps: 0,
            tool_errors: 0,
            loop_warnings: 0,
            loop_aborts: 0,
            patch_failures: 0,
            successful_calls: 0,
            completed: false,
            cost_chars: 0,
        }
    }
}

/// A proposed self-improvement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Improvement {
    /// What to improve
    pub title: String,
    /// Why — what telemetry signal triggered this
    pub reason: String,
    /// Suggested approach
    pub approach: String,
    /// Priority: 1 (critical) to 5 (nice-to-have)
    pub priority: u8,
    /// Which file(s) to modify
    pub target_files: Vec<String>,
}

/// Analyze run stats and propose improvements.
pub fn evaluate(stats: &RunStats) -> Vec<Improvement> {
    let mut improvements = Vec::new();

    // High error rate = something is systematically wrong
    if stats.tool_errors > 3 && stats.steps > 0 {
        let error_rate = stats.tool_errors as f64 / stats.steps as f64;
        if error_rate > 0.3 {
            improvements.push(Improvement {
                title: "Reduce tool error rate".into(),
                reason: format!(
                    "{} errors in {} steps ({:.0}% error rate)",
                    stats.tool_errors,
                    stats.steps,
                    error_rate * 100.0
                ),
                approach: "Check error patterns in session log. Common fixes: better error messages, input validation, fallback strategies.".into(),
                priority: 1,
                target_files: vec!["crates/rc-cli/src/agent.rs".into()],
            });
        }
    }

    // apply_patch failures = patch format or matching issues
    if stats.patch_failures > 2 {
        improvements.push(Improvement {
            title: "Fix apply_patch reliability".into(),
            reason: format!("{} patch failures this run", stats.patch_failures),
            approach: "Check apply_patch error messages. Improve context matching, quote handling, or whitespace tolerance.".into(),
            priority: 1,
            target_files: vec!["crates/sgr-agent/src/app_tools/apply_patch.rs".into()],
        });
    }

    // Loop detection = agent is repeating itself
    if stats.loop_warnings > 2 {
        improvements.push(Improvement {
            title: "Reduce agent loops".into(),
            reason: format!(
                "{} loop warnings, {} aborts",
                stats.loop_warnings, stats.loop_aborts
            ),
            approach: "Analyze which actions loop. Add better error feedback, earlier detection, or alternative strategies in system prompt.".into(),
            priority: 2,
            target_files: vec![
                "crates/rc-cli/src/agent.rs".into(),
                "crates/sgr-agent/src/loop_detect.rs".into(),
            ],
        });
    }

    // Too many steps = inefficient
    if stats.completed && stats.steps > 20 {
        improvements.push(Improvement {
            title: "Reduce step count".into(),
            reason: format!(
                "Task took {} steps (target: <15)",
                stats.steps
            ),
            approach: "Use parallel actions more aggressively. Combine read+edit into fewer steps. Improve system prompt for directness.".into(),
            priority: 3,
            target_files: vec!["crates/rc-cli/src/agent.rs".into()],
        });
    }

    // Task didn't complete = fundamental issue
    if !stats.completed && stats.steps > 5 {
        improvements.push(Improvement {
            title: "Fix task completion".into(),
            reason: format!(
                "Task aborted after {} steps without completing",
                stats.steps
            ),
            approach: "Check why agent couldn't finish. Missing tool? Wrong approach? Need better planning phase?".into(),
            priority: 1,
            target_files: vec!["crates/rc-cli/src/agent.rs".into()],
        });
    }

    improvements.sort_by_key(|i| i.priority);
    improvements
}

/// Format improvements as a markdown task list for the agent.
pub fn format_improvements(improvements: &[Improvement]) -> String {
    if improvements.is_empty() {
        return "No improvements needed — run was clean.".into();
    }

    let mut out = String::from("## Self-Improvement Proposals\n\n");
    for (i, imp) in improvements.iter().enumerate() {
        out.push_str(&format!(
            "{}. **[P{}] {}**\n   Reason: {}\n   Approach: {}\n   Files: {}\n\n",
            i + 1,
            imp.priority,
            imp.title,
            imp.reason,
            imp.approach,
            imp.target_files.join(", ")
        ));
    }
    out
}

/// Build a prompt that asks the agent to improve itself.
pub fn evolution_prompt(stats: &RunStats) -> Option<String> {
    let improvements = evaluate(stats);
    if improvements.is_empty() {
        return None;
    }

    let report = format_improvements(&improvements);
    Some(format!(
        "## Self-Evolution Task\n\n\
         Your last run stats: {} steps, {} errors, {} loops, completed={}\n\n\
         {}\n\
         Pick the highest-priority improvement. Read the target file(s), \
         make the minimal change, write tests, run `make check`, commit, \
         and finish with RESTART_AGENT if you modified agent code.",
        stats.steps, stats.tool_errors, stats.loop_warnings, stats.completed, report,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_run_no_improvements() {
        let stats = RunStats {
            steps: 5,
            tool_errors: 0,
            loop_warnings: 0,
            loop_aborts: 0,
            patch_failures: 0,
            successful_calls: 5,
            completed: true,
            cost_chars: 1000,
        };
        assert!(evaluate(&stats).is_empty());
    }

    #[test]
    fn high_error_rate_triggers_improvement() {
        let stats = RunStats {
            steps: 10,
            tool_errors: 5,
            completed: true,
            ..Default::default()
        };
        let imps = evaluate(&stats);
        assert!(!imps.is_empty());
        assert!(imps[0].title.contains("error rate"));
    }

    #[test]
    fn patch_failures_trigger_improvement() {
        let stats = RunStats {
            steps: 10,
            patch_failures: 4,
            completed: true,
            ..Default::default()
        };
        let imps = evaluate(&stats);
        assert!(imps.iter().any(|i| i.title.contains("apply_patch")));
    }

    #[test]
    fn loop_warnings_trigger_improvement() {
        let stats = RunStats {
            steps: 15,
            loop_warnings: 5,
            loop_aborts: 1,
            completed: true,
            ..Default::default()
        };
        let imps = evaluate(&stats);
        assert!(imps.iter().any(|i| i.title.contains("loop")));
    }

    #[test]
    fn too_many_steps_triggers_improvement() {
        let stats = RunStats {
            steps: 30,
            completed: true,
            ..Default::default()
        };
        let imps = evaluate(&stats);
        assert!(imps.iter().any(|i| i.title.contains("step count")));
    }

    #[test]
    fn incomplete_task_triggers_improvement() {
        let stats = RunStats {
            steps: 10,
            completed: false,
            ..Default::default()
        };
        let imps = evaluate(&stats);
        assert!(imps.iter().any(|i| i.title.contains("completion")));
    }

    #[test]
    fn improvements_sorted_by_priority() {
        let stats = RunStats {
            steps: 30,
            tool_errors: 5,
            loop_warnings: 3,
            patch_failures: 3,
            completed: true,
            ..Default::default()
        };
        let imps = evaluate(&stats);
        for w in imps.windows(2) {
            assert!(w[0].priority <= w[1].priority);
        }
    }

    #[test]
    fn evolution_prompt_none_when_clean() {
        let stats = RunStats {
            steps: 5,
            completed: true,
            ..Default::default()
        };
        assert!(evolution_prompt(&stats).is_none());
    }

    #[test]
    fn evolution_prompt_some_when_issues() {
        let stats = RunStats {
            steps: 10,
            tool_errors: 5,
            completed: false,
            ..Default::default()
        };
        let prompt = evolution_prompt(&stats).unwrap();
        assert!(prompt.contains("Self-Evolution"));
        assert!(prompt.contains("RESTART_AGENT"));
    }
}
