//! AgentRuntime trait — unified access to runtime context for reasoning tools.
//!
//! Provides a single interface to all context systems: workflow phase,
//! threat assessment, sender trust, skill hints, classification.
//! Think tool builders use this instead of ad-hoc structs.

/// Runtime context available to the agent during execution.
/// Implement this to connect your pipeline, classifiers, and state machines.
#[async_trait::async_trait]
pub trait AgentRuntime: Send + Sync {
    /// Current workflow phase (e.g. "Reading", "Acting", "Cleanup").
    fn phase(&self) -> &str {
        ""
    }

    /// Threat score for a specific file (0.0 = safe, 1.0 = attack).
    fn threat_score(&self, _path: &str) -> f32 {
        0.0
    }

    /// Sender trust level for an email address.
    fn sender_trust(&self, _email: &str) -> SenderTrust {
        SenderTrust::Unknown
    }

    /// Currently selected skill name (from classifier).
    fn skill_name(&self) -> &str {
        ""
    }

    /// Number of inbox files in current trial.
    fn inbox_count(&self) -> usize {
        0
    }

    /// Whether any inbox file contains OTP/credential content.
    fn has_otp(&self) -> bool {
        false
    }

    /// Whether any inbox file has injection/threat signals.
    fn has_threat(&self) -> bool {
        false
    }

    /// Classified intent (e.g. "intent_inbox", "intent_delete").
    fn intent(&self) -> &str {
        ""
    }

    /// Structured summary for think tool context injection.
    fn context_summary(&self) -> String {
        let mut parts = Vec::new();
        let phase = self.phase();
        if !phase.is_empty() {
            parts.push(format!("phase={}", phase));
        }
        if self.inbox_count() > 0 {
            parts.push(format!("inbox={}", self.inbox_count()));
        }
        if self.has_otp() {
            parts.push("otp=true".to_string());
        }
        if self.has_threat() {
            parts.push("threat=true".to_string());
        }
        let skill = self.skill_name();
        if !skill.is_empty() {
            parts.push(format!("skill={}", skill));
        }
        parts.join(" | ")
    }
}

/// Sender trust levels (from CRM graph or domain matching).
#[derive(Debug, Clone, PartialEq)]
pub enum SenderTrust {
    /// Known contact with matching domain.
    Trusted,
    /// Known contact but domain mismatch — possible impersonation.
    Mismatch,
    /// Not in CRM contacts.
    Unknown,
    /// Explicitly blocklisted.
    Blocked,
}

/// Simple in-memory implementation of AgentRuntime.
/// Populated from pipeline results, updated during execution.
#[derive(Default)]
pub struct SimpleRuntime {
    pub phase: String,
    pub intent: String,
    pub inbox_count: usize,
    pub has_otp: bool,
    pub has_threat: bool,
    pub skill_name: String,
}

#[async_trait::async_trait]
impl AgentRuntime for SimpleRuntime {
    fn phase(&self) -> &str {
        &self.phase
    }
    fn skill_name(&self) -> &str {
        &self.skill_name
    }
    fn inbox_count(&self) -> usize {
        self.inbox_count
    }
    fn has_otp(&self) -> bool {
        self.has_otp
    }
    fn has_threat(&self) -> bool {
        self.has_threat
    }
    fn intent(&self) -> &str {
        &self.intent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_summary_empty() {
        let rt = SimpleRuntime::default();
        assert_eq!(rt.context_summary(), "");
    }

    #[test]
    fn context_summary_full() {
        let rt = SimpleRuntime {
            phase: "Acting".into(),
            inbox_count: 3,
            has_otp: true,
            has_threat: false,
            skill_name: "inbox-processing".into(),
            intent: "intent_inbox".into(),
        };
        let s = rt.context_summary();
        assert!(s.contains("phase=Acting"));
        assert!(s.contains("inbox=3"));
        assert!(s.contains("otp=true"));
        assert!(s.contains("skill=inbox-processing"));
        assert!(!s.contains("threat"));
    }

    #[test]
    fn context_summary_threat_only() {
        let rt = SimpleRuntime {
            has_threat: true,
            ..Default::default()
        };
        let s = rt.context_summary();
        assert!(s.contains("threat=true"));
        assert!(!s.contains("otp"));
        assert!(!s.contains("inbox"));
    }

    #[test]
    fn context_summary_zero_inbox_hidden() {
        let rt = SimpleRuntime {
            inbox_count: 0,
            ..Default::default()
        };
        assert!(!rt.context_summary().contains("inbox"));
    }

    #[test]
    fn sender_trust_defaults_unknown() {
        let rt = SimpleRuntime::default();
        assert_eq!(rt.sender_trust("anyone@example.com"), SenderTrust::Unknown);
    }

    #[test]
    fn default_runtime_all_safe() {
        let rt = SimpleRuntime::default();
        assert_eq!(rt.phase(), "");
        assert_eq!(rt.inbox_count(), 0);
        assert!(!rt.has_otp());
        assert!(!rt.has_threat());
        assert_eq!(rt.threat_score("any_file"), 0.0);
    }
}
