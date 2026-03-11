//! Loop detection for agent loops.
//!
//! Detects four types of repetitive behavior:
//!
//! 1. **Exact repetition** — identical action signatures (catches trivial loops)
//! 2. **Semantic repetition** — normalized signatures via [`normalize_signature`]
//!    (catches loops where the agent retries the same intent with different flags,
//!    quotes, or fallback chains)
//! 3. **Output stagnation** — identical tool outputs despite varied commands
//!    (catches loops where the agent tries different approaches but gets the same result)
//! 4. **Frequency churn** — same action appearing too often in a sliding window
//!    (catches alternating patterns like `cat X` → `pwd` → `cat X` → `pwd`)
//!
//! Each signal tracks independently. The worst signal determines the returned
//! [`LoopStatus`].

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

// ---------------------------------------------------------------------------
// Signature normalization
// ---------------------------------------------------------------------------

/// Normalize an action signature to its semantic category.
///
/// Strips syntactic noise from bash commands to detect loops where the agent
/// retries the same intent with minor variations (different flags, quotes,
/// fallback chains).
///
/// # Rules
///
/// For `bash:...` signatures:
/// 1. Strip fallback/chain operators (`||`, `&&`, `;`, `|` — with surrounding spaces)
/// 2. Remove command flags (`-n`, `-i`, `--long-flag`)
/// 3. Strip surrounding quotes (`'`, `"`) and trailing slashes from arguments
/// 4. Search tools (`rg`, `grep`, `ag`, `ack`, `fgrep`, `egrep`) → `bash-search:args`
/// 5. Other commands → `bash:cmd:args`
///
/// Non-bash signatures pass through unchanged.
///
/// # Examples
///
/// ```
/// use baml_agent::loop_detect::normalize_signature;
///
/// // All these normalize to the same category:
/// let a = normalize_signature("bash:rg -n 'TODO|FIXME' crates/src/");
/// let b = normalize_signature("bash:rg -Hn \"TODO|FIXME\" crates/src/");
/// let c = normalize_signature("bash:grep -rnE 'TODO|FIXME' crates/src/ || echo 'not found'");
/// assert_eq!(a, b);
/// assert_eq!(b, c);
/// assert_eq!(a, "bash-search:TODO|FIXME crates/src");
///
/// // Non-bash unchanged
/// assert_eq!(normalize_signature("read:src/main.rs"), "read:src/main.rs");
/// ```
pub fn normalize_signature(sig: &str) -> String {
    if let Some(cmd) = sig.strip_prefix("bash:") {
        normalize_bash(cmd)
    } else {
        sig.to_string()
    }
}

/// Binaries recognized as "search" tools (normalized to `bash-search:` category).
const SEARCH_BINS: &[&str] = &["rg", "grep", "ag", "ack", "fgrep", "egrep"];

fn normalize_bash(cmd: &str) -> String {
    // 1. Strip chain operators to isolate the primary command.
    //    Use " || ", " && ", " ; ", " | " (with spaces) to avoid matching
    //    inside quoted patterns like 'TODO|FIXME'.
    let core = [" || ", " && ", " ; ", " | "]
        .iter()
        .fold(cmd, |acc, sep| acc.split(sep).next().unwrap_or(acc))
        .trim();

    // 2. Tokenize by whitespace.
    let tokens: Vec<&str> = core.split_whitespace().collect();
    if tokens.is_empty() {
        return "bash:".into();
    }

    let bin = tokens[0];

    // 3. Extract non-flag arguments, strip quotes and trailing slashes.
    let args: Vec<String> = tokens[1..]
        .iter()
        .filter(|t| !t.starts_with('-'))
        .map(|t| {
            t.trim_matches(|c: char| c == '\'' || c == '"')
                .trim_end_matches('/')
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect();

    // 4. Categorize.
    if SEARCH_BINS.contains(&bin) {
        format!("bash-search:{}", args.join(" "))
    } else if args.is_empty() {
        format!("bash:{}", bin)
    } else {
        format!("bash:{}:{}", bin, args.join(" "))
    }
}

// ---------------------------------------------------------------------------
// Internal trackers
// ---------------------------------------------------------------------------

/// Tracks consecutive occurrences of the same string value.
struct ConsecutiveTracker {
    last: Option<String>,
    count: usize,
}

impl ConsecutiveTracker {
    fn new() -> Self {
        Self {
            last: None,
            count: 0,
        }
    }

    /// Record a value. Returns the current consecutive count (≥ 1).
    fn record(&mut self, value: &str) -> usize {
        if self.last.as_deref() == Some(value) {
            self.count += 1;
        } else {
            self.last = Some(value.to_string());
            self.count = 1;
        }
        self.count
    }

    fn reset(&mut self) {
        self.last = None;
        self.count = 0;
    }

    fn count(&self) -> usize {
        self.count
    }
}

/// Tracks consecutive occurrences by hash (for large strings like tool output).
struct HashTracker {
    last_hash: Option<u64>,
    count: usize,
}

impl HashTracker {
    fn new() -> Self {
        Self {
            last_hash: None,
            count: 0,
        }
    }

    fn record(&mut self, value: &str) -> usize {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        let hash = hasher.finish();

        if self.last_hash == Some(hash) {
            self.count += 1;
        } else {
            self.last_hash = Some(hash);
            self.count = 1;
        }
        self.count
    }

    fn reset(&mut self) {
        self.last_hash = None;
        self.count = 0;
    }

    fn count(&self) -> usize {
        self.count
    }
}

/// Tracks frequency of values in a sliding window.
/// Catches alternating loops like A→B→A→B where consecutive tracking fails.
struct FrequencyTracker {
    window: Vec<String>,
    window_size: usize,
}

impl FrequencyTracker {
    fn new(window_size: usize) -> Self {
        Self {
            window: Vec::new(),
            window_size,
        }
    }

    /// Record a value and return the max frequency of any single value in the window.
    fn record(&mut self, value: &str) -> usize {
        self.window.push(value.to_string());
        if self.window.len() > self.window_size {
            self.window.remove(0);
        }
        self.max_frequency()
    }

    /// Max frequency of any value in the current window.
    fn max_frequency(&self) -> usize {
        if self.window.is_empty() {
            return 0;
        }
        let mut counts = std::collections::HashMap::<&str, usize>::new();
        for v in &self.window {
            *counts.entry(v.as_str()).or_insert(0) += 1;
        }
        counts.values().copied().max().unwrap_or(0)
    }

    fn reset(&mut self) {
        self.window.clear();
    }
}

// ---------------------------------------------------------------------------
// LoopDetector
// ---------------------------------------------------------------------------

/// Detects repeated action patterns in agent loops.
///
/// Four independent signals:
///
/// | Signal    | Tracks                          | Catches                                |
/// |-----------|---------------------------------|----------------------------------------|
/// | Exact     | Consecutive identical sigs      | Trivial loops (same tool, same args)   |
/// | Category  | Consecutive normalized sigs     | Semantic loops (same intent, diff syntax)|
/// | Output    | Consecutive identical output    | Stagnation (different tools, same result)|
/// | Frequency | Sliding window sig frequency    | Churn (alternating A→B→A→B patterns)   |
///
/// Usage:
/// ```ignore
/// let mut detector = LoopDetector::new(6);
///
/// // Per step: check action signatures
/// let sig = "bash:rg -n 'TODO' src/";
/// let cat = normalize_signature(sig);
/// match detector.check_with_category(sig, &cat) {
///     LoopStatus::Abort(n) => { /* stop */ }
///     LoopStatus::Warning(n) => { /* inject system message */ }
///     LoopStatus::Ok => { /* proceed */ }
/// }
///
/// // Per action execution: check tool output
/// match detector.record_output("No matches found") {
///     LoopStatus::Warning(n) => { /* nudge model */ }
///     _ => {}
/// }
/// ```
pub struct LoopDetector {
    /// Tier 1: exact signature repetition.
    exact: ConsecutiveTracker,
    /// Tier 2: normalized category repetition.
    category: ConsecutiveTracker,
    /// Tier 3: tool output repetition (by hash).
    output: HashTracker,
    /// Tier 4: frequency in sliding window (catches alternating patterns).
    frequency: FrequencyTracker,
    abort_threshold: usize,
    warn_threshold: usize,
}

#[derive(Debug, PartialEq)]
pub enum LoopStatus {
    /// No loop detected.
    Ok,
    /// Repeat detected, below abort threshold. Contains repeat count.
    Warning(usize),
    /// Too many repeats, should abort. Contains repeat count.
    Abort(usize),
}

impl LoopDetector {
    /// Create detector. Warns at `⌈abort_threshold/2⌉`, aborts at `abort_threshold`.
    /// Frequency window = `abort_threshold * 2` (wider window to catch alternating patterns).
    pub fn new(abort_threshold: usize) -> Self {
        Self {
            exact: ConsecutiveTracker::new(),
            category: ConsecutiveTracker::new(),
            output: HashTracker::new(),
            frequency: FrequencyTracker::new(abort_threshold * 2),
            abort_threshold,
            warn_threshold: abort_threshold.div_ceil(2),
        }
    }

    /// Create detector with explicit warn threshold.
    pub fn with_thresholds(warn_threshold: usize, abort_threshold: usize) -> Self {
        Self {
            exact: ConsecutiveTracker::new(),
            category: ConsecutiveTracker::new(),
            output: HashTracker::new(),
            frequency: FrequencyTracker::new(abort_threshold * 2),
            abort_threshold,
            warn_threshold,
        }
    }

    /// Check action signature only (backward-compatible).
    ///
    /// Uses `signature` as both exact match and category.
    /// For semantic loop detection, use [`check_with_category`] instead.
    pub fn check(&mut self, signature: &str) -> LoopStatus {
        self.check_with_category(signature, signature)
    }

    /// Check action with separate exact signature and normalized category.
    ///
    /// Returns the worst status across exact, category, and frequency signals.
    pub fn check_with_category(&mut self, signature: &str, category: &str) -> LoopStatus {
        let exact_n = self.exact.record(signature);
        let cat_n = self.category.record(category);
        let freq_n = self.frequency.record(category);
        let max_n = exact_n.max(cat_n).max(freq_n);

        if max_n >= self.abort_threshold {
            LoopStatus::Abort(max_n)
        } else if max_n >= self.warn_threshold {
            LoopStatus::Warning(max_n)
        } else {
            LoopStatus::Ok
        }
    }

    /// Record a tool output and check for output stagnation.
    ///
    /// Call after each action execution. Returns [`LoopStatus::Warning`] or
    /// [`LoopStatus::Abort`] if the same output has been seen too many
    /// consecutive times — the model is retrying a command that keeps
    /// giving the same result.
    pub fn record_output(&mut self, output: &str) -> LoopStatus {
        let n = self.output.record(output);
        if n >= self.abort_threshold {
            LoopStatus::Abort(n)
        } else if n >= self.warn_threshold {
            LoopStatus::Warning(n)
        } else {
            LoopStatus::Ok
        }
    }

    /// Reset all detector state.
    pub fn reset(&mut self) {
        self.exact.reset();
        self.category.reset();
        self.output.reset();
        self.frequency.reset();
    }

    /// Current repeat count (max across all signals).
    pub fn repeat_count(&self) -> usize {
        self.exact
            .count()
            .max(self.category.count())
            .max(self.output.count())
            .max(self.frequency.max_frequency())
    }

    /// Exact signature repeat count.
    pub fn exact_count(&self) -> usize {
        self.exact.count()
    }

    /// Normalized category repeat count.
    pub fn category_count(&self) -> usize {
        self.category.count()
    }

    /// Output stagnation repeat count.
    pub fn output_count(&self) -> usize {
        self.output.count()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_signature ---

    #[test]
    fn normalize_bash_search_strips_flags_and_quotes() {
        assert_eq!(
            normalize_signature("bash:rg -n 'TODO|FIXME' crates/src/"),
            "bash-search:TODO|FIXME crates/src"
        );
    }

    #[test]
    fn normalize_bash_search_double_quotes() {
        assert_eq!(
            normalize_signature("bash:rg -Hn \"TODO|FIXME\" crates/src/"),
            "bash-search:TODO|FIXME crates/src"
        );
    }

    #[test]
    fn normalize_bash_search_strips_fallback() {
        assert_eq!(
            normalize_signature("bash:rg 'TODO' dir/ || echo 'not found'"),
            "bash-search:TODO dir"
        );
    }

    #[test]
    fn normalize_bash_grep_same_as_rg() {
        assert_eq!(
            normalize_signature("bash:grep -rnE 'TODO|FIXME' src/"),
            "bash-search:TODO|FIXME src"
        );
    }

    #[test]
    fn normalize_bash_complex_fallback() {
        assert_eq!(
            normalize_signature("bash:rg 'TODO' dir/ || (echo 'fail' && ls -la dir/)"),
            "bash-search:TODO dir"
        );
    }

    #[test]
    fn normalize_non_bash_unchanged() {
        assert_eq!(normalize_signature("read:src/main.rs"), "read:src/main.rs");
        assert_eq!(
            normalize_signature("write:config.toml"),
            "write:config.toml"
        );
        assert_eq!(normalize_signature("edit:src/lib.rs"), "edit:src/lib.rs");
    }

    #[test]
    fn normalize_bash_non_search_command() {
        assert_eq!(normalize_signature("bash:cargo test"), "bash:cargo:test");
        assert_eq!(normalize_signature("bash:ls -la /tmp"), "bash:ls:/tmp");
        assert_eq!(normalize_signature("bash:cat file.rs"), "bash:cat:file.rs");
    }

    #[test]
    fn normalize_all_rg_variants_equal() {
        let variants = [
            "bash:rg -n 'TODO|FIXME' crates/baml-agent/src/",
            "bash:rg 'TODO|FIXME' crates/baml-agent/src/",
            "bash:rg -i 'TODO|FIXME' crates/baml-agent/src/",
            "bash:rg -Hn \"TODO|FIXME\" crates/baml-agent/src/",
            "bash:rg -n \"TODO|FIXME\" crates/baml-agent/src/ || echo 'No matches'",
            "bash:rg 'TODO|FIXME' crates/baml-agent/src/ || (echo 'fail' && ls -la)",
        ];
        let normalized: Vec<String> = variants.iter().map(|v| normalize_signature(v)).collect();
        let expected = "bash-search:TODO|FIXME crates/baml-agent/src";
        for (i, n) in normalized.iter().enumerate() {
            assert_eq!(n, expected, "variant {} failed: {}", i, variants[i]);
        }
    }

    // --- Exact repetition (backward compat) ---

    #[test]
    fn no_loop_different_sigs() {
        let mut d = LoopDetector::new(6);
        assert_eq!(d.check("a"), LoopStatus::Ok);
        assert_eq!(d.check("b"), LoopStatus::Ok);
        assert_eq!(d.check("c"), LoopStatus::Ok);
    }

    #[test]
    fn warn_then_abort() {
        let mut d = LoopDetector::new(6);
        assert_eq!(d.check("x"), LoopStatus::Ok);
        assert_eq!(d.check("x"), LoopStatus::Ok); // 2
        assert_eq!(d.check("x"), LoopStatus::Warning(3)); // warn at ceil(6/2)=3
        assert_eq!(d.check("x"), LoopStatus::Warning(4));
        assert_eq!(d.check("x"), LoopStatus::Warning(5));
        assert_eq!(d.check("x"), LoopStatus::Abort(6)); // abort at 6
    }

    #[test]
    fn reset_clears() {
        let mut d = LoopDetector::new(4);
        d.check("x");
        d.check("x");
        d.check("x"); // warning
        d.reset();
        assert_eq!(d.check("x"), LoopStatus::Ok); // fresh start
    }

    #[test]
    fn different_sig_resets_consecutive_count() {
        let mut d = LoopDetector::new(6);
        d.check("x");
        d.check("x");
        d.check("x"); // 3 = warning (consecutive + frequency)
        let status = d.check("y");
        // Consecutive resets to 1, but frequency window still has 3 x's → max is 3
        assert_eq!(status, LoopStatus::Warning(3));
        assert_eq!(d.exact_count(), 1); // consecutive reset
    }

    // --- Category (semantic) detection ---

    #[test]
    fn category_catches_semantic_loop() {
        let mut d = LoopDetector::new(4); // warn at 2, abort at 4
                                          // Different exact signatures, same normalized category
        let sigs = [
            "bash:rg -n 'TODO' src/",
            "bash:rg 'TODO' src/",
            "bash:rg -i 'TODO' src/",
            "bash:grep -rn 'TODO' src/",
        ];

        let results: Vec<LoopStatus> = sigs
            .iter()
            .map(|sig| {
                let cat = normalize_signature(sig);
                d.check_with_category(sig, &cat)
            })
            .collect();

        // All exact sigs differ → exact count stays at 1.
        // All categories same → category count 1, 2, 3, 4.
        // max(exact, category) determines result.
        assert_eq!(results[0], LoopStatus::Ok); // max(1,1) = 1 < 2
        assert_eq!(results[1], LoopStatus::Warning(2)); // max(1,2) = 2
        assert_eq!(results[2], LoopStatus::Warning(3)); // max(1,3) = 3
        assert_eq!(results[3], LoopStatus::Abort(4)); // max(1,4) = 4
    }

    #[test]
    fn different_categories_reset() {
        let mut d = LoopDetector::new(4);
        d.check_with_category("bash:rg 'A' src/", "bash-search:A src");
        d.check_with_category("bash:rg 'A' src/", "bash-search:A src"); // cat=2
                                                                        // Different category resets
        d.check_with_category("bash:cargo test", "bash:cargo:test");
        assert_eq!(d.category.count(), 1);
    }

    // --- Output stagnation ---

    #[test]
    fn output_stagnation_detected() {
        let mut d = LoopDetector::new(4); // warn at 2
        assert_eq!(d.record_output("No matches found"), LoopStatus::Ok);
        assert_eq!(d.record_output("No matches found"), LoopStatus::Warning(2));
        assert_eq!(d.record_output("No matches found"), LoopStatus::Warning(3));
        assert_eq!(d.record_output("No matches found"), LoopStatus::Abort(4));
    }

    #[test]
    fn output_different_resets() {
        let mut d = LoopDetector::new(4);
        d.record_output("result A");
        d.record_output("result A"); // 2 = warning
        assert_eq!(d.record_output("result B"), LoopStatus::Ok); // reset to 1
    }

    // --- Frequency churn (alternating patterns) ---

    #[test]
    fn frequency_catches_alternating_pattern() {
        // Simulates: cat X → pwd → cat X → pwd → cat X → pwd
        // Consecutive detector misses this, but frequency catches it.
        let mut d = LoopDetector::new(6); // warn at 3, abort at 6

        // cat and pwd alternate — consecutive count stays at 1 for each
        let sigs = [
            ("bash:cat src/types/index.ts", "bash:cat:src/types/index.ts"),
            ("bash:pwd", "bash:pwd"),
            ("bash:cat src/types/index.ts", "bash:cat:src/types/index.ts"),
            ("bash:pwd", "bash:pwd"),
            ("bash:cat src/types/index.ts", "bash:cat:src/types/index.ts"),
            ("bash:pwd", "bash:pwd"),
        ];

        let mut statuses = Vec::new();
        for (sig, cat) in &sigs {
            statuses.push(d.check_with_category(sig, cat));
        }

        // By step 5 (3rd cat), frequency of "bash:cat:src/types/index.ts" = 3 = warn_threshold
        assert_eq!(statuses[0], LoopStatus::Ok);
        assert_eq!(statuses[1], LoopStatus::Ok);
        assert_eq!(statuses[2], LoopStatus::Ok); // freq=2 < 3
        assert_eq!(statuses[3], LoopStatus::Ok); // freq(pwd)=2 < 3
        assert_eq!(statuses[4], LoopStatus::Warning(3)); // freq(cat)=3 = warn
        assert_eq!(statuses[5], LoopStatus::Warning(3)); // freq(pwd)=3 = warn
    }

    #[test]
    fn frequency_aborts_heavy_churn() {
        let mut d = LoopDetector::new(4); // warn at 2, abort at 4, window=8

        // Alternate cat/pwd 8 times = 4 of each
        for i in 0..8 {
            let (sig, cat) = if i % 2 == 0 {
                ("bash:cat file.ts", "bash:cat:file.ts")
            } else {
                ("bash:pwd", "bash:pwd")
            };
            let status = d.check_with_category(sig, cat);
            if i == 7 {
                // 4th pwd, freq=4 = abort
                assert_eq!(status, LoopStatus::Abort(4));
            }
        }
    }

    // --- Combined: real-world scenario ---

    #[test]
    fn semantic_loop_caught_within_threshold() {
        // Simulates the actual TODO/FIXME loop from testing.
        // 6 steps, each with different flags/quotes but same intent.
        let mut d = LoopDetector::new(6); // warn at 3, abort at 6

        let steps: Vec<(&str, &str)> = vec![
            ("bash:rg \"TODO|FIXME\" crates/baml-agent/src/", ""),
            ("bash:rg -n 'TODO|FIXME' crates/baml-agent/src/", ""),
            (
                "bash:rg -n \"TODO|FIXME\" crates/baml-agent/src/ || echo 'No'",
                "No TODO or FIXME found",
            ),
            (
                "bash:rg 'TODO|FIXME' crates/baml-agent/src/ || (echo && ls)",
                "Search failed...",
            ),
            (
                "bash:rg 'TODO|FIXME' crates/baml-agent/src/",
                "No TODO or FIXME found",
            ),
            (
                "bash:rg -n 'TODO|FIXME' crates/baml-agent/src/ || echo 'No'",
                "No TODO or FIXME found",
            ),
        ];

        let mut first_warning = None;
        let mut abort_at = None;

        for (i, (sig, output)) in steps.iter().enumerate() {
            let cat = normalize_signature(sig);
            match d.check_with_category(sig, &cat) {
                LoopStatus::Warning(n) => {
                    if first_warning.is_none() {
                        first_warning = Some(i + 1);
                    }
                    let _ = n;
                }
                LoopStatus::Abort(_) => {
                    abort_at = Some(i + 1);
                    break;
                }
                LoopStatus::Ok => {}
            }
            d.record_output(output);
        }

        assert_eq!(first_warning, Some(3), "should warn at step 3");
        assert_eq!(abort_at, Some(6), "should abort at step 6");
    }
}
