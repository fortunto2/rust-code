//! LLM-based context compaction — summarize old messages to stay within limits.
//!
//! Unlike simple sliding window (trim_messages), compaction preserves key decisions
//! and file changes by summarizing old messages through a fast LLM.

use crate::client::LlmClient;
use crate::types::Message;

/// Compacts conversation history using LLM summarization.
pub struct Compactor {
    /// Token threshold — compact when estimated tokens exceed this.
    pub threshold: usize,
    /// Number of recent messages to keep uncompacted.
    pub keep_recent: usize,
    /// Number of initial messages to preserve (system + first user).
    pub keep_start: usize,
}

impl Compactor {
    /// Create a compactor with the given token threshold.
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold,
            keep_recent: 10,
            keep_start: 2,
        }
    }

    /// Create with custom keep parameters.
    pub fn with_keep(mut self, start: usize, recent: usize) -> Self {
        self.keep_start = start;
        self.keep_recent = recent;
        self
    }

    /// Check if compaction is needed based on estimated token count.
    pub fn needs_compaction(&self, messages: &[Message]) -> bool {
        estimate_tokens(messages) > self.threshold
    }

    /// Compact messages using an LLM summarizer.
    ///
    /// Replaces messages[keep_start..len-keep_recent] with a single summary message.
    /// Returns true if compaction was performed.
    pub async fn compact(
        &self,
        summarizer: &dyn LlmClient,
        messages: &mut Vec<Message>,
    ) -> Result<bool, CompactionError> {
        let est = estimate_tokens(messages);
        if est <= self.threshold {
            return Ok(false);
        }

        let total = messages.len();
        if total <= self.keep_start + self.keep_recent + 1 {
            // Not enough messages to compact
            return Ok(false);
        }

        let compact_end = total - self.keep_recent;
        let to_compact = &messages[self.keep_start..compact_end];

        if to_compact.is_empty() {
            return Ok(false);
        }

        // Format messages for summarization
        let formatted = format_messages_for_summary(to_compact);

        let summary_prompt = vec![
            Message::system(COMPACTION_PROMPT),
            Message::user(&formatted),
        ];

        let summary = summarizer
            .complete(&summary_prompt)
            .await
            .map_err(|e| CompactionError::Llm(e.to_string()))?;

        if summary.is_empty() {
            return Err(CompactionError::EmptySummary);
        }

        // Replace compacted messages with summary
        let compacted_count = compact_end - self.keep_start;
        messages.drain(self.keep_start..compact_end);
        messages.insert(
            self.keep_start,
            Message::system(format!(
                "<compacted count=\"{}\">\n{}\n</compacted>",
                compacted_count, summary
            )),
        );

        Ok(true)
    }
}

impl Default for Compactor {
    fn default() -> Self {
        // ~100K tokens threshold (roughly 400K chars / 4)
        Self::new(100_000)
    }
}

/// Errors from compaction.
#[derive(Debug)]
pub enum CompactionError {
    /// LLM call failed.
    Llm(String),
    /// LLM returned empty summary.
    EmptySummary,
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Llm(e) => write!(f, "Compaction LLM error: {}", e),
            Self::EmptySummary => write!(f, "LLM returned empty summary"),
        }
    }
}

impl std::error::Error for CompactionError {}

/// Estimate token count for messages (rough: chars / 4).
/// Uses char count (not byte length) for correct non-ASCII estimation.
pub fn estimate_tokens(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| m.content.chars().count() / 4 + 1)
        .sum()
}

/// System prompt for the summarizer LLM.
const COMPACTION_PROMPT: &str = r#"Summarize this conversation concisely. Preserve:
- Key decisions made
- Files read, created, or modified (with paths)
- Important findings and errors encountered
- Current task state and next steps

Be concise but thorough. Use bullet points. Do not lose critical context."#;

/// Format messages into text for summarization.
fn format_messages_for_summary(messages: &[Message]) -> String {
    let mut output = String::new();
    for msg in messages {
        let role = match msg.role {
            crate::types::Role::System => "SYSTEM",
            crate::types::Role::User => "USER",
            crate::types::Role::Assistant => "ASSISTANT",
            crate::types::Role::Tool => "TOOL",
        };
        // Truncate very long messages for summarization (char-safe boundary)
        let content = if msg.content.chars().count() > 2000 {
            let truncated: String = msg.content.chars().take(2000).collect();
            format!(
                "{}... [truncated, {} chars total]",
                truncated,
                msg.content.chars().count()
            )
        } else {
            msg.content.clone()
        };
        output.push_str(&format!("[{}]: {}\n\n", role, content));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_basic() {
        let msgs = vec![
            Message::system("Hello world"), // 11 chars → 3 tokens
            Message::user("How are you"),   // 11 chars → 3 tokens
        ];
        let est = estimate_tokens(&msgs);
        assert!(est > 0);
        assert!(est < 100);
    }

    #[test]
    fn estimate_tokens_non_ascii() {
        // "Привет" = 6 chars, 12 bytes. chars/4 = 1, bytes/4 = 3
        let msgs = vec![Message::user("Привет мир")]; // 10 chars
        let est = estimate_tokens(&msgs);
        // Should be 10/4+1 = 3, not 20/4+1 = 6
        assert_eq!(est, 3);
    }

    #[test]
    fn format_messages_non_ascii_truncation() {
        // 3000 Russian chars — should not panic
        let cyrillic: String = "Б".repeat(3000);
        let msgs = vec![Message::user(&cyrillic)];
        let formatted = format_messages_for_summary(&msgs);
        assert!(formatted.contains("truncated"));
    }

    #[test]
    fn needs_compaction_under_threshold() {
        let compactor = Compactor::new(1000);
        let msgs = vec![Message::user("short")];
        assert!(!compactor.needs_compaction(&msgs));
    }

    #[test]
    fn needs_compaction_over_threshold() {
        let compactor = Compactor::new(10);
        let msgs: Vec<Message> = (0..100)
            .map(|i| Message::user(&format!("Message number {} with some content to pad it out", i)))
            .collect();
        assert!(compactor.needs_compaction(&msgs));
    }

    #[test]
    fn format_messages_truncates_long() {
        let long_msg = "x".repeat(5000);
        let msgs = vec![Message::user(&long_msg)];
        let formatted = format_messages_for_summary(&msgs);
        assert!(formatted.contains("truncated"));
        assert!(formatted.len() < 5000);
    }

    #[test]
    fn compactor_default() {
        let c = Compactor::default();
        assert_eq!(c.threshold, 100_000);
        assert_eq!(c.keep_recent, 10);
        assert_eq!(c.keep_start, 2);
    }

    #[test]
    fn compactor_with_keep() {
        let c = Compactor::new(50_000).with_keep(3, 5);
        assert_eq!(c.keep_start, 3);
        assert_eq!(c.keep_recent, 5);
    }

    #[tokio::test]
    async fn compact_not_needed() {
        use crate::types::SgrError;
        struct MockClient;
        #[async_trait::async_trait]
        impl LlmClient for MockClient {
            async fn structured_call(
                &self, _: &[Message], _: &serde_json::Value,
            ) -> Result<(Option<serde_json::Value>, Vec<crate::types::ToolCall>, String), SgrError> {
                unimplemented!()
            }
            async fn tools_call(
                &self, _: &[Message], _: &[crate::tool::ToolDef],
            ) -> Result<Vec<crate::types::ToolCall>, SgrError> {
                unimplemented!()
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                Ok("Summary of conversation.".into())
            }
        }

        let compactor = Compactor::new(100_000);
        let mut msgs = vec![Message::user("short")];
        let result = compactor.compact(&MockClient, &mut msgs).await.unwrap();
        assert!(!result);
        assert_eq!(msgs.len(), 1);
    }

    #[tokio::test]
    async fn compact_replaces_old_messages() {
        use crate::types::SgrError;
        struct MockClient;
        #[async_trait::async_trait]
        impl LlmClient for MockClient {
            async fn structured_call(
                &self, _: &[Message], _: &serde_json::Value,
            ) -> Result<(Option<serde_json::Value>, Vec<crate::types::ToolCall>, String), SgrError> {
                unimplemented!()
            }
            async fn tools_call(
                &self, _: &[Message], _: &[crate::tool::ToolDef],
            ) -> Result<Vec<crate::types::ToolCall>, SgrError> {
                unimplemented!()
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                Ok("Key decisions: implemented auth module. Files: src/auth.rs created.".into())
            }
        }

        let compactor = Compactor::new(5).with_keep(2, 2); // very low threshold
        let mut msgs = vec![
            Message::system("System prompt"),
            Message::user("Initial task"),
            Message::assistant("Step 1 done"),
            Message::user("Continue"),
            Message::assistant("Step 2 done"),
            Message::user("Continue more"),
            Message::assistant("Step 3 done"),
            // last 2 to keep:
            Message::user("Final step"),
            Message::assistant("All done"),
        ];

        let result = compactor.compact(&MockClient, &mut msgs).await.unwrap();
        assert!(result);

        // Should have: system, initial, compacted summary, last 2
        assert_eq!(msgs.len(), 5);
        assert!(msgs[2].content.contains("compacted"));
        assert!(msgs[2].content.contains("Key decisions"));
        assert_eq!(msgs[3].content, "Final step");
        assert_eq!(msgs[4].content, "All done");
    }
}
