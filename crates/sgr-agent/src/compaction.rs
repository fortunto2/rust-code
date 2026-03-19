//! LLM-based context compaction — summarize old messages to stay within limits.
//!
//! Unlike simple sliding window (trim_messages), compaction preserves key decisions
//! and file changes by summarizing old messages through a fast LLM.

use crate::client::LlmClient;
use crate::types::Message;

#[cfg(feature = "session")]
use crate::session::{AgentMessage, MessageRole, Session};

/// Compacts conversation history using LLM summarization.
pub struct Compactor {
    /// Token threshold — compact when estimated tokens exceed this.
    pub threshold: usize,
    /// Number of recent messages to keep uncompacted.
    pub keep_recent: usize,
    /// Number of initial messages to preserve (system + first user).
    pub keep_start: usize,
    /// Custom compaction prompt (overrides the default).
    prompt: Option<String>,
}

impl Compactor {
    /// Create a compactor with the given token threshold.
    pub fn new(threshold: usize) -> Self {
        Self {
            threshold,
            keep_recent: 10,
            keep_start: 2,
            prompt: None,
        }
    }

    /// Create with custom keep parameters.
    pub fn with_keep(mut self, start: usize, recent: usize) -> Self {
        self.keep_start = start;
        self.keep_recent = recent;
        self
    }

    /// Use a custom compaction prompt instead of the default.
    ///
    /// Useful for domain-specific compaction (e.g., sales coaching, code review).
    pub fn with_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.prompt = Some(prompt.into());
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

        let prompt = self.prompt.as_deref().unwrap_or(COMPACTION_PROMPT);
        let summary_prompt = vec![Message::system(prompt), Message::user(&formatted)];

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

/// Session-aware compaction methods (requires `session` feature).
#[cfg(feature = "session")]
impl Compactor {
    /// Estimate token count for a session's messages.
    pub fn estimate_session_tokens<M: AgentMessage>(session: &Session<M>) -> usize {
        session
            .messages()
            .iter()
            .map(|m: &M| m.content().chars().count() / 4 + 1)
            .sum()
    }

    /// Check if compaction is needed for a session.
    pub fn needs_session_compaction<M: AgentMessage>(&self, session: &Session<M>) -> bool {
        Self::estimate_session_tokens(session) > self.threshold
    }

    /// Compact a session using LLM summarization with incremental support.
    ///
    /// Preserves prior `<compacted>` summaries verbatim — only summarizes new
    /// messages since the last compaction. Returns the number of messages compacted.
    pub async fn compact_session<M: AgentMessage>(
        &self,
        summarizer: &dyn LlmClient,
        session: &mut Session<M>,
    ) -> Result<usize, CompactionError> {
        if !self.needs_session_compaction(session) {
            return Ok(0);
        }

        let total = session.messages().len();
        if total <= self.keep_start + self.keep_recent + 1 {
            return Ok(0);
        }

        let compact_end = total - self.keep_recent;
        let to_compact = &session.messages()[self.keep_start..compact_end];
        if to_compact.is_empty() {
            return Ok(0);
        }

        // Separate existing compacted summaries from new messages (incremental).
        let mut prior_summary: Option<String> = None;
        let mut new_messages: Vec<(&str, &str)> = Vec::new();
        for m in to_compact.iter() {
            let content: &str = m.content();
            if content.starts_with("<compacted") {
                prior_summary = Some(content.to_string());
            } else {
                new_messages.push((m.role().as_str(), content));
            }
        }

        // Format new messages for summarization
        let formatted = format_agent_messages_for_summary(&new_messages);
        let compacted_count = compact_end - self.keep_start;

        // Build user content with prior summary if incremental
        let user_content = match &prior_summary {
            Some(prev) => format!(
                "Previous summary (preserve verbatim, do not re-summarize):\n{prev}\n\nNew messages to summarize:\n{formatted}"
            ),
            None => formatted,
        };

        let prompt = self.prompt.as_deref().unwrap_or(COMPACTION_PROMPT);
        let summary_prompt = vec![Message::system(prompt), Message::user(&user_content)];

        let summary = summarizer
            .complete(&summary_prompt)
            .await
            .map_err(|e| CompactionError::Llm(e.to_string()))?;

        if summary.is_empty() {
            return Err(CompactionError::EmptySummary);
        }

        // Replace compacted messages with summary
        let msgs = session.messages_mut();
        msgs.drain(self.keep_start..compact_end);
        let summary_content =
            format!("<compacted turns=\"{compacted_count}\">\n{summary}\n</compacted>");
        msgs.insert(self.keep_start, M::new(M::Role::system(), summary_content));

        Ok(compacted_count)
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

/// Format generic agent messages (role string + content) for summarization.
#[cfg(feature = "session")]
fn format_agent_messages_for_summary(messages: &[(&str, &str)]) -> String {
    let mut output = String::new();
    for (role, content) in messages {
        let label = match *role {
            "system" => "SYSTEM",
            "user" => "USER",
            "assistant" => "ASSISTANT",
            "tool" => "TOOL",
            other => other,
        };
        let content = if content.chars().count() > 2000 {
            let truncated: String = content.chars().take(2000).collect();
            format!(
                "{}... [truncated, {} chars total]",
                truncated,
                content.chars().count()
            )
        } else {
            content.to_string()
        };
        output.push_str(&format!("[{}]: {}\n\n", label, content));
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
            .map(|i| {
                Message::user(format!(
                    "Message number {} with some content to pad it out",
                    i
                ))
            })
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
                &self,
                _: &[Message],
                _: &serde_json::Value,
            ) -> Result<
                (
                    Option<serde_json::Value>,
                    Vec<crate::types::ToolCall>,
                    String,
                ),
                SgrError,
            > {
                unimplemented!()
            }
            async fn tools_call(
                &self,
                _: &[Message],
                _: &[crate::tool::ToolDef],
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
                &self,
                _: &[Message],
                _: &serde_json::Value,
            ) -> Result<
                (
                    Option<serde_json::Value>,
                    Vec<crate::types::ToolCall>,
                    String,
                ),
                SgrError,
            > {
                unimplemented!()
            }
            async fn tools_call(
                &self,
                _: &[Message],
                _: &[crate::tool::ToolDef],
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

    #[test]
    fn with_prompt_overrides_default() {
        let c = Compactor::new(1000).with_prompt("Custom: summarize sales data");
        assert_eq!(c.prompt.as_deref(), Some("Custom: summarize sales data"));
    }

    #[tokio::test]
    async fn compact_uses_custom_prompt() {
        use crate::types::SgrError;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let saw_custom = Arc::new(AtomicBool::new(false));
        let saw_custom_clone = saw_custom.clone();

        struct PromptCheckClient {
            saw_custom: Arc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl LlmClient for PromptCheckClient {
            async fn structured_call(
                &self,
                _: &[Message],
                _: &serde_json::Value,
            ) -> Result<
                (
                    Option<serde_json::Value>,
                    Vec<crate::types::ToolCall>,
                    String,
                ),
                SgrError,
            > {
                unimplemented!()
            }
            async fn tools_call(
                &self,
                _: &[Message],
                _: &[crate::tool::ToolDef],
            ) -> Result<Vec<crate::types::ToolCall>, SgrError> {
                unimplemented!()
            }
            async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
                if messages[0].content.contains("SALES FOCUS") {
                    self.saw_custom.store(true, Ordering::SeqCst);
                }
                Ok("Summary".into())
            }
        }

        let client = PromptCheckClient {
            saw_custom: saw_custom_clone,
        };
        let compactor = Compactor::new(5)
            .with_keep(1, 1)
            .with_prompt("SALES FOCUS: summarize this");

        let mut msgs = vec![
            Message::system("sys"),
            Message::user("msg1"),
            Message::assistant("resp1"),
            Message::user("msg2"),
            Message::assistant("resp2"),
            Message::user("last"),
        ];

        let result = compactor.compact(&client, &mut msgs).await.unwrap();
        assert!(result);
        assert!(saw_custom.load(Ordering::SeqCst));
    }

    #[cfg(feature = "session")]
    mod session_tests {
        use super::*;
        use crate::session::Session;
        use crate::session::simple::{SimpleMsg, SimpleRole};

        fn make_session() -> Session<SimpleMsg> {
            let dir = std::env::temp_dir().join("sgr_compact_session_test");
            let _ = std::fs::remove_dir_all(&dir);
            Session::new(dir.to_str().unwrap(), 100).unwrap()
        }

        #[test]
        fn estimate_session_tokens_basic() {
            let mut session = make_session();
            session.push(SimpleRole::User, "Hello world".into()); // 11 chars → 3
            session.push(SimpleRole::Assistant, "Hi there".into()); // 8 chars → 3
            let est = Compactor::estimate_session_tokens(&session);
            assert!(est > 0 && est < 100);
            let dir = std::env::temp_dir().join("sgr_compact_session_test");
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn compact_session_not_needed() {
            use crate::types::SgrError;
            struct MockClient;
            #[async_trait::async_trait]
            impl LlmClient for MockClient {
                async fn structured_call(
                    &self,
                    _: &[Message],
                    _: &serde_json::Value,
                ) -> Result<
                    (
                        Option<serde_json::Value>,
                        Vec<crate::types::ToolCall>,
                        String,
                    ),
                    SgrError,
                > {
                    unimplemented!()
                }
                async fn tools_call(
                    &self,
                    _: &[Message],
                    _: &[crate::tool::ToolDef],
                ) -> Result<Vec<crate::types::ToolCall>, SgrError> {
                    unimplemented!()
                }
                async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                    Ok("summary".into())
                }
            }

            let mut session = make_session();
            session.push(SimpleRole::User, "short msg".into());
            let compactor = Compactor::new(100_000);
            let result = compactor
                .compact_session(&MockClient, &mut session)
                .await
                .unwrap();
            assert_eq!(result, 0);
            let dir = std::env::temp_dir().join("sgr_compact_session_test");
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn compact_session_replaces_middle() {
            use crate::types::SgrError;
            struct MockClient;
            #[async_trait::async_trait]
            impl LlmClient for MockClient {
                async fn structured_call(
                    &self,
                    _: &[Message],
                    _: &serde_json::Value,
                ) -> Result<
                    (
                        Option<serde_json::Value>,
                        Vec<crate::types::ToolCall>,
                        String,
                    ),
                    SgrError,
                > {
                    unimplemented!()
                }
                async fn tools_call(
                    &self,
                    _: &[Message],
                    _: &[crate::tool::ToolDef],
                ) -> Result<Vec<crate::types::ToolCall>, SgrError> {
                    unimplemented!()
                }
                async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                    Ok("Compacted: auth module created".into())
                }
            }

            let mut session = make_session();
            session.push(SimpleRole::System, "system prompt".into());
            session.push(SimpleRole::User, "initial task".into());
            for i in 0..6 {
                let role = if i % 2 == 0 {
                    SimpleRole::User
                } else {
                    SimpleRole::Assistant
                };
                session.push(role, format!("msg {i}"));
            }
            session.push(SimpleRole::User, "final".into());
            session.push(SimpleRole::Assistant, "done".into());

            let compactor = Compactor::new(5).with_keep(2, 2);
            let result = compactor
                .compact_session(&MockClient, &mut session)
                .await
                .unwrap();
            assert!(result > 0);

            // Check structure: keep_start(2) + compacted(1) + keep_recent(2) = 5
            assert_eq!(session.messages().len(), 5);
            assert!(session.messages()[2].content().contains("<compacted"));
            assert!(session.messages()[2].content().contains("auth module"));
            assert_eq!(session.messages()[3].content(), "final");
            assert_eq!(session.messages()[4].content(), "done");

            let dir = std::env::temp_dir().join("sgr_compact_session_test");
            let _ = std::fs::remove_dir_all(&dir);
        }

        #[tokio::test]
        async fn compact_session_incremental_preserves_prior() {
            use crate::types::SgrError;
            use std::sync::Arc;
            use std::sync::atomic::{AtomicBool, Ordering};

            let saw_prior = Arc::new(AtomicBool::new(false));
            let saw_prior_clone = saw_prior.clone();

            struct IncrementalClient {
                saw_prior: Arc<AtomicBool>,
            }
            #[async_trait::async_trait]
            impl LlmClient for IncrementalClient {
                async fn structured_call(
                    &self,
                    _: &[Message],
                    _: &serde_json::Value,
                ) -> Result<
                    (
                        Option<serde_json::Value>,
                        Vec<crate::types::ToolCall>,
                        String,
                    ),
                    SgrError,
                > {
                    unimplemented!()
                }
                async fn tools_call(
                    &self,
                    _: &[Message],
                    _: &[crate::tool::ToolDef],
                ) -> Result<Vec<crate::types::ToolCall>, SgrError> {
                    unimplemented!()
                }
                async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
                    // Check that the user message contains the prior summary
                    if messages[1].content.contains("Previous summary")
                        && messages[1].content.contains("prior context here")
                    {
                        self.saw_prior.store(true, Ordering::SeqCst);
                    }
                    Ok("Merged summary".into())
                }
            }

            let mut session = make_session();
            session.push(SimpleRole::System, "system".into());
            session.push(SimpleRole::User, "initial".into());
            // Simulate a prior compacted block in the middle
            session.push(
                SimpleRole::System,
                "<compacted turns=\"5\">\nprior context here\n</compacted>".into(),
            );
            for i in 0..4 {
                let role = if i % 2 == 0 {
                    SimpleRole::User
                } else {
                    SimpleRole::Assistant
                };
                session.push(role, format!("new msg {i}"));
            }
            session.push(SimpleRole::User, "keep1".into());
            session.push(SimpleRole::Assistant, "keep2".into());

            let client = IncrementalClient {
                saw_prior: saw_prior_clone,
            };
            let compactor = Compactor::new(5).with_keep(2, 2);
            let result = compactor
                .compact_session(&client, &mut session)
                .await
                .unwrap();

            assert!(result > 0);
            assert!(
                saw_prior.load(Ordering::SeqCst),
                "should send prior summary to LLM"
            );
            assert!(session.messages()[2].content().contains("<compacted"));

            let dir = std::env::temp_dir().join("sgr_compact_session_test");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
