//! Benchmark suite: 5 fixed tasks to measure agent quality.
//!
//! Each task is deterministic — same input, measurable output.
//! Score = average across all tasks (0.0–1.0).
//!
//! Run after every self-evolution patch to detect regressions.
//!
//! Inspired by Karpathy's autoresearch: fixed budget, single metric.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single benchmark task.
#[derive(Debug, Clone)]
pub struct BenchmarkTask {
    /// Short name
    pub name: &'static str,
    /// Prompt sent to agent
    pub prompt: &'static str,
    /// Max steps budget (Karpathy: fixed budget per experiment)
    pub max_steps: usize,
    /// How to verify success — function checks the output
    pub verify: fn(&BenchmarkResult) -> f64,
}

/// Result of running one benchmark task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResult {
    pub name: String,
    pub steps: usize,
    pub completed: bool,
    pub tool_errors: usize,
    pub loop_warnings: usize,
    /// Agent's final output (finish summary)
    pub output: String,
    /// Score for this task (0.0–1.0)
    pub score: f64,
}

/// Full benchmark report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub timestamp: u64,
    pub commit: String,
    pub results: Vec<BenchmarkResult>,
    /// Average score across all tasks
    pub avg_score: f64,
    /// Standard deviation (Knuth: measure uncertainty)
    pub std_dev: f64,
}

// ---------------------------------------------------------------------------
// The 5 benchmark tasks
// ---------------------------------------------------------------------------

/// Task 1: Simple Q&A — can agent use finish tool correctly?
const TASK_QA: BenchmarkTask = BenchmarkTask {
    name: "qa_simple",
    prompt: "What is the capital of France? Answer with the finish tool.",
    max_steps: 3,
    verify: verify_qa,
};

fn verify_qa(r: &BenchmarkResult) -> f64 {
    if !r.completed {
        return 0.0;
    }
    let has_paris = r.output.to_lowercase().contains("paris");
    let efficiency = if r.steps <= 1 {
        1.0
    } else {
        0.8 / r.steps as f64
    };
    if has_paris {
        (0.7 + efficiency * 0.3).min(1.0)
    } else {
        0.1
    }
}

/// Task 2: File read — can agent read a file and extract info?
const TASK_READ: BenchmarkTask = BenchmarkTask {
    name: "read_file",
    prompt: "Read the file Cargo.toml in the current directory and tell me the package name. Use finish tool with the name.",
    max_steps: 5,
    verify: verify_read,
};

fn verify_read(r: &BenchmarkResult) -> f64 {
    if !r.completed {
        return 0.0;
    }
    // Should find "rust-code" or whatever the package name is
    let output_lower = r.output.to_lowercase();
    let found_name = output_lower.contains("rust-code")
        || output_lower.contains("sgr-agent")
        || output_lower.contains("package");
    let efficiency = (3.0 / r.steps.max(1) as f64).min(1.0);
    if found_name {
        0.6 + efficiency * 0.4
    } else if r.completed {
        0.3
    } else {
        0.0
    }
}

/// Task 3: Code search — can agent find something in the codebase?
const TASK_SEARCH: BenchmarkTask = BenchmarkTask {
    name: "code_search",
    prompt: "Search for the function 'parse_spec' in the codebase and tell me which file it's in. Use finish tool.",
    max_steps: 8,
    verify: verify_search,
};

fn verify_search(r: &BenchmarkResult) -> f64 {
    if !r.completed {
        return 0.0;
    }
    let output_lower = r.output.to_lowercase();
    let found = output_lower.contains("openapi")
        || output_lower.contains("spec.rs")
        || output_lower.contains("parse_spec");
    let no_errors = r.tool_errors == 0;
    let efficiency = (5.0 / r.steps.max(1) as f64).min(1.0);
    let mut score = 0.0;
    if found {
        score += 0.5;
    }
    if no_errors {
        score += 0.2;
    }
    score += efficiency * 0.3;
    score.min(1.0)
}

/// Task 4: Multi-step — can agent do read + analyze + answer?
const TASK_MULTI: BenchmarkTask = BenchmarkTask {
    name: "multi_step",
    prompt: "Read crates/sgr-agent/src/lib.rs, count how many pub mod declarations it has, and answer with the count using finish tool.",
    max_steps: 10,
    verify: verify_multi,
};

fn verify_multi(r: &BenchmarkResult) -> f64 {
    if !r.completed {
        return 0.0;
    }
    // Should have a number in the output
    let has_number = r.output.chars().any(|c| c.is_ascii_digit());
    let no_loops = r.loop_warnings == 0;
    let efficiency = (4.0 / r.steps.max(1) as f64).min(1.0);
    let mut score = 0.0;
    if has_number {
        score += 0.5;
    }
    if no_loops {
        score += 0.2;
    }
    score += efficiency * 0.3;
    score.min(1.0)
}

/// Task 5: Tool chaining — can agent use git_status + analysis?
const TASK_GIT: BenchmarkTask = BenchmarkTask {
    name: "git_status",
    prompt: "Check git status of this repo. Tell me which branch we're on and if there are uncommitted changes. Use finish tool.",
    max_steps: 5,
    verify: verify_git,
};

fn verify_git(r: &BenchmarkResult) -> f64 {
    if !r.completed {
        return 0.0;
    }
    let output_lower = r.output.to_lowercase();
    let has_branch = output_lower.contains("master")
        || output_lower.contains("main")
        || output_lower.contains("branch");
    let has_status = output_lower.contains("clean")
        || output_lower.contains("uncommitted")
        || output_lower.contains("modified")
        || output_lower.contains("changes");
    let efficiency = (3.0 / r.steps.max(1) as f64).min(1.0);
    let mut score = 0.0;
    if has_branch {
        score += 0.35;
    }
    if has_status {
        score += 0.35;
    }
    score += efficiency * 0.3;
    score.min(1.0)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// All 5 benchmark tasks.
pub fn all_tasks() -> Vec<BenchmarkTask> {
    vec![TASK_QA, TASK_READ, TASK_SEARCH, TASK_MULTI, TASK_GIT]
}

/// Compute aggregate report from individual results.
pub fn compute_report(results: Vec<BenchmarkResult>, commit: &str) -> BenchmarkReport {
    let n = results.len() as f64;
    let avg = if n > 0.0 {
        results.iter().map(|r| r.score).sum::<f64>() / n
    } else {
        0.0
    };
    let variance = if n > 1.0 {
        results.iter().map(|r| (r.score - avg).powi(2)).sum::<f64>() / (n - 1.0)
    } else {
        0.0
    };
    let std_dev = variance.sqrt();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    BenchmarkReport {
        timestamp: ts,
        commit: commit.to_string(),
        results,
        avg_score: avg,
        std_dev,
    }
}

/// Format report for display (Knuth: literate output).
pub fn format_report(report: &BenchmarkReport) -> String {
    let mut out = format!(
        "## Benchmark Report\n\n\
         Commit: {} | Score: {:.3} ± {:.3}\n\n\
         | Task | Steps | Errors | Score | Status |\n\
         |------|-------|--------|-------|--------|\n",
        report.commit, report.avg_score, report.std_dev,
    );
    for r in &report.results {
        let status = if r.score >= 0.8 {
            "✓"
        } else if r.score >= 0.5 {
            "~"
        } else {
            "✗"
        };
        out.push_str(&format!(
            "| {} | {} | {} | {:.2} | {} |\n",
            r.name, r.steps, r.tool_errors, r.score, status,
        ));
    }
    out.push_str(&format!(
        "\n**Average: {:.3} ± {:.3}**\n",
        report.avg_score, report.std_dev,
    ));
    out
}

/// Save benchmark report to JSONL log.
pub fn log_benchmark(agent_home: &str, report: &BenchmarkReport) -> Result<(), String> {
    let path = Path::new(agent_home).join("benchmark.jsonl");
    let line = serde_json::to_string(report).map_err(|e| format!("serialize: {}", e))?;
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("open: {}", e))?;
    writeln!(f, "{}", line).map_err(|e| format!("write: {}", e))?;
    Ok(())
}

/// Load benchmark history.
pub fn load_benchmarks(agent_home: &str) -> Vec<BenchmarkReport> {
    let path = Path::new(agent_home).join("benchmark.jsonl");
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

/// Compare two reports: did we improve? (Karpathy: keep/discard decision)
pub fn compare(before: &BenchmarkReport, after: &BenchmarkReport) -> &'static str {
    if after.avg_score > before.avg_score + before.std_dev * 0.5 {
        "keep" // statistically significant improvement
    } else if after.avg_score < before.avg_score - before.std_dev * 0.5 {
        "discard" // regression
    } else {
        "neutral" // within noise
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_tasks_has_five() {
        assert_eq!(all_tasks().len(), 5);
    }

    #[test]
    fn verify_qa_correct() {
        let r = BenchmarkResult {
            name: "qa".into(),
            steps: 1,
            completed: true,
            tool_errors: 0,
            loop_warnings: 0,
            output: "The capital of France is Paris.".into(),
            score: 0.0,
        };
        let s = verify_qa(&r);
        assert!(
            s > 0.9,
            "correct answer in 1 step should score >0.9, got {}",
            s
        );
    }

    #[test]
    fn verify_qa_wrong() {
        let r = BenchmarkResult {
            name: "qa".into(),
            steps: 1,
            completed: true,
            tool_errors: 0,
            loop_warnings: 0,
            output: "I don't know".into(),
            score: 0.0,
        };
        assert!(verify_qa(&r) < 0.5);
    }

    #[test]
    fn verify_qa_not_completed() {
        let r = BenchmarkResult {
            name: "qa".into(),
            steps: 3,
            completed: false,
            tool_errors: 1,
            loop_warnings: 0,
            output: "".into(),
            score: 0.0,
        };
        assert_eq!(verify_qa(&r), 0.0);
    }

    #[test]
    fn compute_report_avg_and_stddev() {
        let results = vec![
            BenchmarkResult {
                name: "a".into(),
                steps: 1,
                completed: true,
                tool_errors: 0,
                loop_warnings: 0,
                output: "".into(),
                score: 0.8,
            },
            BenchmarkResult {
                name: "b".into(),
                steps: 2,
                completed: true,
                tool_errors: 0,
                loop_warnings: 0,
                output: "".into(),
                score: 0.6,
            },
        ];
        let report = compute_report(results, "abc123");
        assert!((report.avg_score - 0.7).abs() < 0.001);
        assert!(report.std_dev > 0.0);
    }

    #[test]
    fn compare_improvement() {
        let before = BenchmarkReport {
            timestamp: 0,
            commit: "a".into(),
            results: vec![],
            avg_score: 0.5,
            std_dev: 0.1,
        };
        let after = BenchmarkReport {
            timestamp: 1,
            commit: "b".into(),
            results: vec![],
            avg_score: 0.7,
            std_dev: 0.1,
        };
        assert_eq!(compare(&before, &after), "keep");
    }

    #[test]
    fn compare_regression() {
        let before = BenchmarkReport {
            timestamp: 0,
            commit: "a".into(),
            results: vec![],
            avg_score: 0.8,
            std_dev: 0.05,
        };
        let after = BenchmarkReport {
            timestamp: 1,
            commit: "b".into(),
            results: vec![],
            avg_score: 0.6,
            std_dev: 0.05,
        };
        assert_eq!(compare(&before, &after), "discard");
    }

    #[test]
    fn compare_neutral() {
        let before = BenchmarkReport {
            timestamp: 0,
            commit: "a".into(),
            results: vec![],
            avg_score: 0.7,
            std_dev: 0.15,
        };
        let after = BenchmarkReport {
            timestamp: 1,
            commit: "b".into(),
            results: vec![],
            avg_score: 0.72,
            std_dev: 0.15,
        };
        assert_eq!(compare(&before, &after), "neutral");
    }

    #[test]
    fn format_report_markdown() {
        let report = BenchmarkReport {
            timestamp: 0,
            commit: "abc123".into(),
            results: vec![BenchmarkResult {
                name: "test".into(),
                steps: 2,
                completed: true,
                tool_errors: 0,
                loop_warnings: 0,
                output: "done".into(),
                score: 0.9,
            }],
            avg_score: 0.9,
            std_dev: 0.0,
        };
        let md = format_report(&report);
        assert!(md.contains("abc123"));
        assert!(md.contains("0.900"));
        assert!(md.contains("test"));
    }

    #[test]
    fn log_and_load_benchmarks() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_str().unwrap();
        let report = BenchmarkReport {
            timestamp: 12345,
            commit: "test".into(),
            results: vec![],
            avg_score: 0.75,
            std_dev: 0.1,
        };
        log_benchmark(home, &report).unwrap();
        let history = load_benchmarks(home);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].avg_score, 0.75);
    }
}
