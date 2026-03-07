//! Cost tracking: estimate tokens and cost per step/session.

use std::sync::atomic::{AtomicUsize, Ordering};

/// Global session cost tracker.
static TOTAL_INPUT_CHARS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_OUTPUT_CHARS: AtomicUsize = AtomicUsize::new(0);
static TOTAL_STEPS: AtomicUsize = AtomicUsize::new(0);

/// Record a step's input/output sizes.
pub fn record_step(input_chars: usize, output_chars: usize) {
    TOTAL_INPUT_CHARS.fetch_add(input_chars, Ordering::Relaxed);
    TOTAL_OUTPUT_CHARS.fetch_add(output_chars, Ordering::Relaxed);
    TOTAL_STEPS.fetch_add(1, Ordering::Relaxed);
}

/// Get current session stats.
pub fn session_stats() -> CostStats {
    let input_chars = TOTAL_INPUT_CHARS.load(Ordering::Relaxed);
    let output_chars = TOTAL_OUTPUT_CHARS.load(Ordering::Relaxed);
    let steps = TOTAL_STEPS.load(Ordering::Relaxed);

    // Rough estimate: 1 token ≈ 4 chars (English), conservative for code
    let input_tokens = input_chars / 4;
    let output_tokens = output_chars / 4;

    CostStats {
        steps,
        input_tokens,
        output_tokens,
        input_chars,
        output_chars,
    }
}

/// Reset counters (for new session).
pub fn reset_cost() {
    TOTAL_INPUT_CHARS.store(0, Ordering::Relaxed);
    TOTAL_OUTPUT_CHARS.store(0, Ordering::Relaxed);
    TOTAL_STEPS.store(0, Ordering::Relaxed);
}

#[derive(Debug, Clone)]
pub struct CostStats {
    pub steps: usize,
    pub input_tokens: usize,
    pub output_tokens: usize,
    pub input_chars: usize,
    pub output_chars: usize,
}

impl CostStats {
    /// Estimate cost in USD based on model pricing.
    /// Default: Gemini 2.5 Flash pricing ($0.15/1M input, $0.60/1M output).
    pub fn estimated_cost_usd(&self) -> f64 {
        let input_cost = self.input_tokens as f64 * 0.15 / 1_000_000.0;
        let output_cost = self.output_tokens as f64 * 0.60 / 1_000_000.0;
        input_cost + output_cost
    }

    /// Format as compact status line for TUI.
    pub fn status_line(&self) -> String {
        let cost = self.estimated_cost_usd();
        if cost < 0.001 {
            format!(
                "{}→{}tok | {} steps | <$0.001",
                fmt_tokens(self.input_tokens),
                fmt_tokens(self.output_tokens),
                self.steps
            )
        } else {
            format!(
                "{}→{}tok | {} steps | ${:.3}",
                fmt_tokens(self.input_tokens),
                fmt_tokens(self.output_tokens),
                self.steps,
                cost
            )
        }
    }
}

fn fmt_tokens(t: usize) -> String {
    if t >= 1_000_000 {
        format!("{:.1}M", t as f64 / 1_000_000.0)
    } else if t >= 1_000 {
        format!("{:.1}K", t as f64 / 1_000.0)
    } else {
        format!("{}", t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_tracking_works() {
        reset_cost();
        record_step(4000, 1000); // ~1K input, ~250 output tokens
        record_step(8000, 2000);

        let stats = session_stats();
        assert_eq!(stats.steps, 2);
        assert_eq!(stats.input_tokens, 3000); // 12000 / 4
        assert_eq!(stats.output_tokens, 750); // 3000 / 4
        assert!(stats.estimated_cost_usd() > 0.0);
        assert!(stats.estimated_cost_usd() < 0.01);

        let line = stats.status_line();
        assert!(line.contains("3.0K"));
        assert!(line.contains("2 steps"));
        reset_cost();
    }

    #[test]
    fn fmt_tokens_formatting() {
        assert_eq!(fmt_tokens(500), "500");
        assert_eq!(fmt_tokens(1500), "1.5K");
        assert_eq!(fmt_tokens(1_500_000), "1.5M");
    }
}
