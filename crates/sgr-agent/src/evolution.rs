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
use std::path::{Path, PathBuf};
use std::time::Instant;

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

// ---------------------------------------------------------------------------
// Score: single number to measure agent efficiency
// ---------------------------------------------------------------------------

/// Efficiency score: 0.0 (terrible) to 1.0 (perfect).
/// `successful_calls / steps` weighted by completion.
pub fn score(stats: &RunStats) -> f64 {
    if stats.steps == 0 {
        return 0.0;
    }
    let efficiency = stats.successful_calls as f64 / stats.steps as f64;
    let completion_bonus = if stats.completed { 1.0 } else { 0.5 };
    let loop_penalty = 1.0 - (stats.loop_warnings as f64 * 0.05).min(0.3);
    (efficiency * completion_bonus * loop_penalty).clamp(0.0, 1.0)
}

// ---------------------------------------------------------------------------
// Evolution log: JSONL record of each experiment
// ---------------------------------------------------------------------------

/// One entry in evolution.jsonl — records a single self-improvement attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvolutionEntry {
    /// ISO timestamp
    pub ts: String,
    /// Git commit hash (short)
    pub commit: String,
    /// What was tried
    pub title: String,
    /// Score before this change
    pub score_before: f64,
    /// Score after this change
    pub score_after: f64,
    /// "keep", "discard", or "crash"
    pub status: String,
    /// Run stats
    pub stats: RunStats,
}

/// Default evolution log path.
pub fn evolution_log_path(agent_home: &str) -> PathBuf {
    PathBuf::from(agent_home).join("evolution.jsonl")
}

/// Append an entry to evolution.jsonl.
pub fn log_evolution(agent_home: &str, entry: &EvolutionEntry) -> Result<(), String> {
    let path = evolution_log_path(agent_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {}", e))?;
    }
    let line = serde_json::to_string(entry).map_err(|e| format!("serialize: {}", e))?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open: {}", e))?;
    writeln!(f, "{}", line).map_err(|e| format!("write: {}", e))?;
    Ok(())
}

/// Load evolution history.
pub fn load_evolution(agent_home: &str) -> Vec<EvolutionEntry> {
    let path = evolution_log_path(agent_home);
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Get the latest score (baseline for comparison).
pub fn baseline_score(agent_home: &str) -> f64 {
    load_evolution(agent_home)
        .last()
        .map(|e| e.score_after)
        .unwrap_or(0.0)
}

/// Count how many "keep" vs "discard" in history.
pub fn evolution_summary(agent_home: &str) -> (usize, usize, usize) {
    let entries = load_evolution(agent_home);
    let keep = entries.iter().filter(|e| e.status == "keep").count();
    let discard = entries.iter().filter(|e| e.status == "discard").count();
    let crash = entries.iter().filter(|e| e.status == "crash").count();
    (keep, discard, crash)
}

// ---------------------------------------------------------------------------
// Improvements
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Session history analysis: learn from past runs
// ---------------------------------------------------------------------------

/// Pattern found across multiple sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPattern {
    /// What pattern was found
    pub pattern: String,
    /// How many times it appeared
    pub count: usize,
    /// Example occurrences (first 3)
    pub examples: Vec<String>,
}

/// Analyze recent session logs for recurring issues.
/// Reads last `max_sessions` JSONL files from agent home dir.
pub fn analyze_sessions(agent_home: &str, max_sessions: usize) -> Vec<SessionPattern> {
    let dir = PathBuf::from(agent_home);
    let mut session_files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().map(|e| e == "jsonl").unwrap_or(false)
                        && p.file_name()
                            .map(|n| n.to_string_lossy().starts_with("session_"))
                            .unwrap_or(false)
                })
                .collect()
        })
        .unwrap_or_default();
    session_files.sort();
    session_files.reverse(); // newest first
    session_files.truncate(max_sessions);

    // Count patterns across all sessions
    let mut patch_errors: Vec<String> = Vec::new();
    let mut loop_warnings: usize = 0;
    let mut tool_errors: Vec<String> = Vec::new();
    let mut reread_warnings: usize = 0;
    let mut total_messages: usize = 0;

    for path in &session_files {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for line in content.lines() {
            let msg: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let text = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");

            if role == "tool" || role == "assistant" {
                total_messages += 1;

                // Patch failures
                if text.contains("apply_patch error") || text.contains("Commit FAILED") {
                    let snippet: String = text.lines().take(2).collect::<Vec<_>>().join(" ");
                    patch_errors.push(truncate_string(&snippet, 100));
                }

                // Loop warnings
                if text.contains("Loop detected") || text.contains("LOOP WARNING") {
                    loop_warnings += 1;
                }

                // Tool errors
                if text.contains("FAILED") || text.starts_with("Error") {
                    let snippet: String = text.lines().next().unwrap_or("").to_string();
                    tool_errors.push(truncate_string(&snippet, 100));
                }

                // Re-read warnings
                if text.contains("RE-READ") || text.contains("already read") {
                    reread_warnings += 1;
                }
            }
        }
    }

    let mut patterns = Vec::new();

    if patch_errors.len() > 2 {
        patterns.push(SessionPattern {
            pattern: format!(
                "apply_patch failures ({} across {} sessions)",
                patch_errors.len(),
                session_files.len()
            ),
            count: patch_errors.len(),
            examples: patch_errors.into_iter().take(3).collect(),
        });
    }

    if loop_warnings > 3 {
        patterns.push(SessionPattern {
            pattern: format!(
                "Loop warnings ({} across {} sessions)",
                loop_warnings,
                session_files.len()
            ),
            count: loop_warnings,
            examples: vec![],
        });
    }

    if tool_errors.len() > 5 {
        // Group by first word to find most common error type
        let mut error_types: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for err in &tool_errors {
            let key = err.split_whitespace().take(3).collect::<Vec<_>>().join(" ");
            *error_types.entry(key).or_insert(0) += 1;
        }
        let mut sorted: Vec<_> = error_types.into_iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(&a.1));

        for (error_type, count) in sorted.into_iter().take(3) {
            if count > 2 {
                patterns.push(SessionPattern {
                    pattern: format!("Recurring error: '{}' ({}x)", error_type, count),
                    count,
                    examples: tool_errors
                        .iter()
                        .filter(|e| e.contains(&error_type.split_whitespace().next().unwrap_or("")))
                        .take(2)
                        .cloned()
                        .collect(),
                });
            }
        }
    }

    if reread_warnings > 3 {
        patterns.push(SessionPattern {
            pattern: format!(
                "File re-reads ({} — agent wastes tokens re-reading)",
                reread_warnings
            ),
            count: reread_warnings,
            examples: vec![],
        });
    }

    patterns.sort_by(|a, b| b.count.cmp(&a.count));
    patterns
}

fn truncate_string(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!(
            "{}...",
            &s[..s
                .char_indices()
                .take(max)
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0)]
        )
    }
}

/// Build an evolution prompt that includes session history analysis.
pub fn evolution_prompt_with_history(stats: &RunStats, agent_home: &str) -> Option<String> {
    let improvements = evaluate(stats);
    let patterns = analyze_sessions(agent_home, 10);

    if improvements.is_empty() && patterns.is_empty() {
        return None;
    }

    let mut prompt = format!(
        "## Self-Evolution Task\n\n\
         Your last run stats: {} steps, {} errors, {} loops, completed={}\n\n",
        stats.steps, stats.tool_errors, stats.loop_warnings, stats.completed,
    );

    if !patterns.is_empty() {
        prompt.push_str("### Recurring Issues (from last 10 sessions)\n\n");
        for p in &patterns {
            prompt.push_str(&format!("- **{}**\n", p.pattern));
            for ex in &p.examples {
                prompt.push_str(&format!("  - `{}`\n", ex));
            }
        }
        prompt.push('\n');
    }

    if !improvements.is_empty() {
        prompt.push_str(&format_improvements(&improvements));
    }

    prompt.push_str(
        "\nPick the highest-priority issue. Read the target file(s), \
         make the minimal change, write tests, run `make check`, commit, \
         and finish with RESTART_AGENT if you modified agent code.",
    );

    Some(prompt)
}

// ---------------------------------------------------------------------------
// Loop engine: BigHead-style autonomous loop (compatible with solo-dev.sh)
// ---------------------------------------------------------------------------

/// Solo-compatible signals in agent output.
/// `<solo:done/>` = stage complete, move to next
/// `<solo:redo/>` = go back to previous stage (e.g. review found issues)
#[derive(Debug, Clone, PartialEq)]
pub enum SoloSignal {
    Done,
    Redo,
    None,
}

/// Parse solo signals from agent output.
pub fn parse_signal(output: &str) -> SoloSignal {
    if output.contains("<solo:done/>") {
        SoloSignal::Done
    } else if output.contains("<solo:redo/>") {
        SoloSignal::Redo
    } else {
        SoloSignal::None
    }
}

/// Control commands via file. Compatible with solo-dev.sh.
/// Write "stop", "pause", or "skip" to the control file.
#[derive(Debug, Clone, PartialEq)]
pub enum ControlAction {
    Continue,
    Stop,
    Pause,
    Skip,
}

/// Check control file for commands. Reads and deletes (except pause which persists).
pub fn check_control(control_path: &Path) -> ControlAction {
    let content = match std::fs::read_to_string(control_path) {
        Ok(c) => c.trim().to_lowercase(),
        Err(_) => return ControlAction::Continue,
    };
    match content.as_str() {
        "stop" => {
            let _ = std::fs::remove_file(control_path);
            ControlAction::Stop
        }
        "pause" => ControlAction::Pause, // don't delete — pause persists
        "skip" => {
            let _ = std::fs::remove_file(control_path);
            ControlAction::Skip
        }
        _ => ControlAction::Continue,
    }
}

/// Circuit breaker: stops after N consecutive identical failures.
#[derive(Debug)]
pub struct CircuitBreaker {
    last_fingerprint: String,
    consecutive: usize,
    limit: usize,
}

impl CircuitBreaker {
    pub fn new(limit: usize) -> Self {
        Self {
            last_fingerprint: String::new(),
            consecutive: 0,
            limit,
        }
    }

    /// Record a result. Returns true if circuit is tripped (should stop).
    pub fn record(&mut self, success: bool, fingerprint: &str) -> bool {
        if success {
            self.consecutive = 0;
            self.last_fingerprint.clear();
            return false;
        }
        if fingerprint == self.last_fingerprint {
            self.consecutive += 1;
        } else {
            self.last_fingerprint = fingerprint.to_string();
            self.consecutive = 1;
        }
        self.consecutive >= self.limit
    }

    pub fn consecutive_failures(&self) -> usize {
        self.consecutive
    }
}

/// Loop configuration (mirrors solo-dev.sh flags).
#[derive(Debug, Clone)]
pub struct LoopOptions {
    /// Max iterations (0 = unlimited)
    pub max_iterations: usize,
    /// Max wall clock hours (0.0 = unlimited)
    pub max_hours: f64,
    /// Control file path (write "stop"/"pause"/"skip")
    pub control_file: PathBuf,
    /// Circuit breaker: max consecutive identical failures
    pub circuit_breaker_limit: usize,
    /// Agent home dir for evolution.jsonl
    pub agent_home: String,
    /// Mode: "loop" (repeat task) or "evolve" (self-improve)
    pub mode: LoopMode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LoopMode {
    /// BigHead: repeat prompt until <solo:done/> or max iterations
    Loop,
    /// Evolution: evaluate → pick improvement → patch → test → commit → restart
    Evolve,
}

impl Default for LoopOptions {
    fn default() -> Self {
        Self {
            max_iterations: 20,
            max_hours: 0.0,
            control_file: PathBuf::from(".rust-code/loop-control"),
            circuit_breaker_limit: 3,
            agent_home: ".rust-code".into(),
            mode: LoopMode::Loop,
        }
    }
}

/// Loop state — tracks progress across iterations.
#[derive(Debug)]
pub struct LoopState {
    pub iteration: usize,
    pub start_time: Instant,
    pub breaker: CircuitBreaker,
    pub options: LoopOptions,
    pub total_score: f64,
    pub keep_count: usize,
    pub discard_count: usize,
}

impl LoopState {
    pub fn new(options: LoopOptions) -> Self {
        let limit = options.circuit_breaker_limit;
        Self {
            iteration: 0,
            start_time: Instant::now(),
            breaker: CircuitBreaker::new(limit),
            options,
            total_score: 0.0,
            keep_count: 0,
            discard_count: 0,
        }
    }

    /// Check if loop should continue. Returns None to continue, Some(reason) to stop.
    pub fn should_stop(&self) -> Option<String> {
        // Max iterations
        if self.options.max_iterations > 0 && self.iteration >= self.options.max_iterations {
            return Some(format!(
                "Max iterations reached ({})",
                self.options.max_iterations
            ));
        }
        // Max hours
        if self.options.max_hours > 0.0 {
            let elapsed_hours = self.start_time.elapsed().as_secs_f64() / 3600.0;
            if elapsed_hours >= self.options.max_hours {
                return Some(format!("Timeout ({:.1}h)", self.options.max_hours));
            }
        }
        // Control file
        match check_control(&self.options.control_file) {
            ControlAction::Stop => return Some("Stop requested via control file".into()),
            ControlAction::Pause => {
                return Some("Paused via control file (delete to resume)".into())
            }
            _ => {}
        }
        None
    }

    /// Record iteration result and check circuit breaker.
    pub fn record_iteration(&mut self, stats: &RunStats) -> bool {
        self.iteration += 1;
        let s = score(stats);
        self.total_score += s;
        let fingerprint = format!(
            "errors:{},loops:{},patches:{}",
            stats.tool_errors, stats.loop_warnings, stats.patch_failures
        );
        let success = stats.completed && stats.tool_errors == 0;
        if success {
            self.keep_count += 1;
        } else {
            self.discard_count += 1;
        }
        // Returns true if circuit is tripped
        self.breaker.record(success, &fingerprint)
    }

    /// Elapsed time as human-readable string.
    pub fn elapsed_display(&self) -> String {
        let secs = self.start_time.elapsed().as_secs();
        if secs < 60 {
            format!("{}s", secs)
        } else if secs < 3600 {
            format!("{}m{}s", secs / 60, secs % 60)
        } else {
            format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
        }
    }

    /// Summary string for display.
    pub fn summary(&self) -> String {
        let avg = if self.iteration > 0 {
            self.total_score / self.iteration as f64
        } else {
            0.0
        };
        format!(
            "{} iterations in {} | keep:{} discard:{} | avg score:{:.3}",
            self.iteration,
            self.elapsed_display(),
            self.keep_count,
            self.discard_count,
            avg,
        )
    }
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

    // --- Session analysis ---

    #[test]
    fn analyze_sessions_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let patterns = analyze_sessions(dir.path().to_str().unwrap(), 10);
        assert!(patterns.is_empty());
    }

    #[test]
    fn analyze_sessions_finds_patch_errors() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        // Write fake session with patch errors
        let session = vec![
            r#"{"role":"user","content":"fix bug"}"#,
            r#"{"role":"tool","content":"apply_patch error: failed to find match"}"#,
            r#"{"role":"tool","content":"apply_patch error: invalid hunk"}"#,
            r#"{"role":"tool","content":"apply_patch error: context mismatch"}"#,
            r#"{"role":"tool","content":"done"}"#,
        ];
        std::fs::write(dir.path().join("session_1000.jsonl"), session.join("\n")).unwrap();

        let patterns = analyze_sessions(home, 10);
        assert!(
            patterns.iter().any(|p| p.pattern.contains("apply_patch")),
            "should find patch errors, got: {:?}",
            patterns
        );
    }

    #[test]
    fn analyze_sessions_finds_loops() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let mut lines = vec![r#"{"role":"user","content":"task"}"#.to_string()];
        for _ in 0..5 {
            lines.push(
                r#"{"role":"tool","content":"LOOP WARNING: Loop detected — 5 repeats"}"#
                    .to_string(),
            );
        }
        std::fs::write(dir.path().join("session_2000.jsonl"), lines.join("\n")).unwrap();

        let patterns = analyze_sessions(home, 10);
        assert!(patterns.iter().any(|p| p.pattern.contains("Loop")));
    }

    #[test]
    fn evolution_prompt_with_history_includes_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let session = vec![
            r#"{"role":"tool","content":"apply_patch error: x"}"#,
            r#"{"role":"tool","content":"apply_patch error: y"}"#,
            r#"{"role":"tool","content":"apply_patch error: z"}"#,
        ];
        std::fs::write(dir.path().join("session_3000.jsonl"), session.join("\n")).unwrap();

        let stats = RunStats {
            steps: 10,
            tool_errors: 5,
            completed: false,
            ..Default::default()
        };
        let prompt = evolution_prompt_with_history(&stats, home).unwrap();
        assert!(prompt.contains("Recurring Issues"));
        assert!(prompt.contains("apply_patch"));
    }

    // --- Solo signals ---

    #[test]
    fn parse_signal_done() {
        assert_eq!(parse_signal("result <solo:done/>"), SoloSignal::Done);
    }

    #[test]
    fn parse_signal_redo() {
        assert_eq!(parse_signal("needs fix <solo:redo/>"), SoloSignal::Redo);
    }

    #[test]
    fn parse_signal_none() {
        assert_eq!(parse_signal("just text"), SoloSignal::None);
    }

    // --- Control file ---

    #[test]
    fn control_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = dir.path().join("control");
        assert_eq!(check_control(&ctrl), ControlAction::Continue);
    }

    #[test]
    fn control_file_stop() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = dir.path().join("control");
        std::fs::write(&ctrl, "stop").unwrap();
        assert_eq!(check_control(&ctrl), ControlAction::Stop);
        assert!(!ctrl.exists()); // deleted after read
    }

    #[test]
    fn control_file_pause() {
        let dir = tempfile::tempdir().unwrap();
        let ctrl = dir.path().join("control");
        std::fs::write(&ctrl, "pause").unwrap();
        assert_eq!(check_control(&ctrl), ControlAction::Pause);
        assert!(ctrl.exists()); // NOT deleted — pause persists
    }

    // --- Circuit breaker ---

    #[test]
    fn circuit_breaker_trips_on_consecutive() {
        let mut cb = CircuitBreaker::new(3);
        assert!(!cb.record(false, "err1"));
        assert!(!cb.record(false, "err1"));
        assert!(cb.record(false, "err1")); // 3rd identical failure → trip
    }

    #[test]
    fn circuit_breaker_resets_on_success() {
        let mut cb = CircuitBreaker::new(3);
        cb.record(false, "err1");
        cb.record(false, "err1");
        cb.record(true, ""); // success resets
        assert_eq!(cb.consecutive_failures(), 0);
        assert!(!cb.record(false, "err1")); // starts over
    }

    #[test]
    fn circuit_breaker_resets_on_different_error() {
        let mut cb = CircuitBreaker::new(3);
        cb.record(false, "err1");
        cb.record(false, "err1");
        assert!(!cb.record(false, "err2")); // different error → reset to 1
        assert_eq!(cb.consecutive_failures(), 1);
    }

    // --- Loop state ---

    #[test]
    fn loop_state_max_iterations() {
        let opts = LoopOptions {
            max_iterations: 3,
            ..Default::default()
        };
        let mut state = LoopState::new(opts);
        assert!(state.should_stop().is_none());
        state.iteration = 3;
        assert!(state.should_stop().is_some());
    }

    #[test]
    fn loop_state_summary() {
        let mut state = LoopState::new(LoopOptions::default());
        state.iteration = 5;
        state.keep_count = 3;
        state.discard_count = 2;
        state.total_score = 4.0;
        let s = state.summary();
        assert!(s.contains("5 iterations"));
        assert!(s.contains("keep:3"));
        assert!(s.contains("discard:2"));
    }

    // --- Score tests ---

    #[test]
    fn score_perfect_run() {
        let stats = RunStats {
            steps: 5,
            successful_calls: 5,
            completed: true,
            ..Default::default()
        };
        let s = score(&stats);
        assert!(s > 0.9, "perfect run score should be >0.9, got {}", s);
    }

    #[test]
    fn score_zero_steps() {
        assert_eq!(score(&RunStats::default()), 0.0);
    }

    #[test]
    fn score_incomplete_penalized() {
        let complete = RunStats {
            steps: 10,
            successful_calls: 8,
            completed: true,
            ..Default::default()
        };
        let incomplete = RunStats {
            steps: 10,
            successful_calls: 8,
            completed: false,
            ..Default::default()
        };
        assert!(score(&complete) > score(&incomplete));
    }

    #[test]
    fn score_loops_penalized() {
        let clean = RunStats {
            steps: 10,
            successful_calls: 8,
            completed: true,
            ..Default::default()
        };
        let loopy = RunStats {
            steps: 10,
            successful_calls: 8,
            completed: true,
            loop_warnings: 5,
            ..Default::default()
        };
        assert!(score(&clean) > score(&loopy));
    }

    #[test]
    fn score_clamped_to_01() {
        let stats = RunStats {
            steps: 1,
            successful_calls: 100, // impossible but tests clamping
            completed: true,
            ..Default::default()
        };
        assert!(score(&stats) <= 1.0);
    }

    // --- JSONL tests ---

    #[test]
    fn log_and_load_evolution() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();

        let entry = EvolutionEntry {
            ts: "2026-03-14T12:00:00Z".into(),
            commit: "abc1234".into(),
            title: "test improvement".into(),
            score_before: 0.5,
            score_after: 0.7,
            status: "keep".into(),
            stats: RunStats {
                steps: 10,
                successful_calls: 8,
                completed: true,
                ..Default::default()
            },
        };

        log_evolution(home, &entry).unwrap();
        log_evolution(home, &entry).unwrap();

        let history = load_evolution(home);
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].title, "test improvement");
        assert_eq!(history[0].score_after, 0.7);
    }

    #[test]
    fn baseline_score_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(baseline_score(dir.path().to_str().unwrap()), 0.0);
    }

    #[test]
    fn baseline_score_from_history() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();

        log_evolution(
            home,
            &EvolutionEntry {
                ts: "t1".into(),
                commit: "a".into(),
                title: "first".into(),
                score_before: 0.0,
                score_after: 0.5,
                status: "keep".into(),
                stats: Default::default(),
            },
        )
        .unwrap();
        log_evolution(
            home,
            &EvolutionEntry {
                ts: "t2".into(),
                commit: "b".into(),
                title: "second".into(),
                score_before: 0.5,
                score_after: 0.8,
                status: "keep".into(),
                stats: Default::default(),
            },
        )
        .unwrap();

        assert_eq!(baseline_score(home), 0.8);
    }

    #[test]
    fn evolution_summary_counts() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let base = EvolutionEntry {
            ts: "t".into(),
            commit: "x".into(),
            title: "x".into(),
            score_before: 0.0,
            score_after: 0.0,
            status: "keep".into(),
            stats: Default::default(),
        };
        log_evolution(home, &base).unwrap();
        log_evolution(
            home,
            &EvolutionEntry {
                status: "discard".into(),
                ..base.clone()
            },
        )
        .unwrap();
        log_evolution(
            home,
            &EvolutionEntry {
                status: "crash".into(),
                ..base.clone()
            },
        )
        .unwrap();
        log_evolution(home, &base).unwrap();

        let (keep, discard, crash) = evolution_summary(home);
        assert_eq!(keep, 2);
        assert_eq!(discard, 1);
        assert_eq!(crash, 1);
    }
}
