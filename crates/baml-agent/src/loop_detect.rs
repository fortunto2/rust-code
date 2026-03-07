/// Detects repeated action patterns in agent loops.
///
/// Usage:
/// ```ignore
/// let mut detector = LoopDetector::new(6); // abort after 6 repeats
/// for action in actions {
///     let sig = format!("tool:{}:{}", action.name, action.key_arg);
///     match detector.check(&sig) {
///         LoopStatus::Ok => { /* proceed */ }
///         LoopStatus::Warning(n) => { /* inject warning into context */ }
///         LoopStatus::Abort(n) => { /* stop the loop */ }
///     }
/// }
/// ```
pub struct LoopDetector {
    last_signature: Option<String>,
    repeat_count: usize,
    abort_threshold: usize,
    warn_threshold: usize,
}

#[derive(Debug, PartialEq)]
pub enum LoopStatus {
    /// No loop detected.
    Ok,
    /// Repeat detected, but below abort threshold. Contains repeat count.
    Warning(usize),
    /// Too many repeats, should abort. Contains repeat count.
    Abort(usize),
}

impl LoopDetector {
    /// Create detector. Warns at `abort_threshold / 2`, aborts at `abort_threshold`.
    pub fn new(abort_threshold: usize) -> Self {
        Self {
            last_signature: None,
            repeat_count: 0,
            abort_threshold,
            warn_threshold: abort_threshold / 2,
        }
    }

    /// Create detector with explicit warn threshold.
    pub fn with_thresholds(warn_threshold: usize, abort_threshold: usize) -> Self {
        Self {
            last_signature: None,
            repeat_count: 0,
            abort_threshold,
            warn_threshold,
        }
    }

    /// Check a combined signature for the current step's actions.
    ///
    /// `signature` should uniquely identify the action(s) being taken.
    /// If multiple actions, join their signatures with `|`.
    pub fn check(&mut self, signature: &str) -> LoopStatus {
        if self.last_signature.as_deref() == Some(signature) {
            self.repeat_count += 1;
            if self.repeat_count >= self.abort_threshold {
                return LoopStatus::Abort(self.repeat_count);
            }
            if self.repeat_count >= self.warn_threshold {
                return LoopStatus::Warning(self.repeat_count);
            }
        } else {
            self.repeat_count = 1;
            self.last_signature = Some(signature.into());
        }
        LoopStatus::Ok
    }

    /// Reset detector state.
    pub fn reset(&mut self) {
        self.last_signature = None;
        self.repeat_count = 0;
    }

    pub fn repeat_count(&self) -> usize {
        self.repeat_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(d.check("x"), LoopStatus::Warning(3)); // warn at 3
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
    fn different_sig_resets_count() {
        let mut d = LoopDetector::new(6);
        d.check("x");
        d.check("x");
        d.check("x"); // 3 = warning
        assert_eq!(d.check("y"), LoopStatus::Ok); // reset
        assert_eq!(d.repeat_count(), 1);
    }
}
