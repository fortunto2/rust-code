//! Stateful session wrapper for OpenAI Responses API server-side caching.
//!
//! Tracks `last_response_id` to send only delta (new messages) per turn,
//! leveraging server-side conversation cache for lower latency and cost.

use super::store::Session;
use super::traits::AgentMessage;

/// Delta computed from a stateful session — what to send for the next API call.
#[derive(Debug, Clone)]
pub struct StatefulDelta {
    /// Previous response ID for server-side cache lookup.
    pub previous_response_id: String,
    /// Index into session messages where new messages start.
    pub msg_offset: usize,
    /// Pending tool outputs from the last cached response.
    pub pending_outputs: Vec<(String, String)>,
}

/// Wraps [`Session<M>`] with OpenAI Responses API statefulness.
///
/// Tracks `last_response_id` to send only new messages (delta) per turn.
/// On API failure, call [`clear`] to fall back to full history.
pub struct StatefulSession<M: AgentMessage> {
    session: Session<M>,
    last_response_id: Option<String>,
    last_msg_count: usize,
    pending_tool_outputs: Vec<(String, String)>,
}

impl<M: AgentMessage> StatefulSession<M> {
    /// Wrap an existing session with stateful tracking.
    pub fn new(session: Session<M>) -> Self {
        Self {
            session,
            last_response_id: None,
            last_msg_count: 0,
            pending_tool_outputs: Vec::new(),
        }
    }

    /// Compute delta messages since last successful call.
    ///
    /// Returns `None` if no previous response is cached (use full history).
    /// When `Some`, the caller should send only messages from `msg_offset`
    /// plus the pending tool outputs, referencing `previous_response_id`.
    pub fn delta(&self) -> Option<StatefulDelta> {
        let id = self.last_response_id.as_ref()?;
        let total = self.session.messages().len();
        if self.last_msg_count == 0 || self.last_msg_count > total {
            return None;
        }
        Some(StatefulDelta {
            previous_response_id: id.clone(),
            msg_offset: self.last_msg_count,
            pending_outputs: self.pending_tool_outputs.clone(),
        })
    }

    /// Store response state after a successful terminal call.
    ///
    /// `response_id` — server-assigned response ID (if stateful API).
    /// `msg_count` — current message count at time of response.
    /// `outputs` — pending tool call outputs to replay on next delta.
    pub fn store_response(
        &mut self,
        response_id: Option<String>,
        msg_count: usize,
        outputs: Vec<(String, String)>,
    ) {
        self.last_response_id = response_id;
        self.last_msg_count = msg_count;
        self.pending_tool_outputs = outputs;
    }

    /// Clear stateful state — next call will use full history.
    pub fn clear(&mut self) {
        self.last_response_id = None;
        self.last_msg_count = 0;
        self.pending_tool_outputs = Vec::new();
    }

    /// Read-only access to the inner session.
    pub fn session(&self) -> &Session<M> {
        &self.session
    }

    /// Mutable access to the inner session.
    pub fn session_mut(&mut self) -> &mut Session<M> {
        &mut self.session
    }

    /// Delegate: push a message to the inner session.
    pub fn push(&mut self, role: <M as AgentMessage>::Role, content: String) -> &M {
        self.session.push(role, content)
    }

    /// Delegate: read messages from the inner session.
    pub fn messages(&self) -> &[M] {
        self.session.messages()
    }

    /// Delegate: check if session is empty.
    pub fn is_empty(&self) -> bool {
        self.session.is_empty()
    }

    /// Delegate: message count.
    pub fn len(&self) -> usize {
        self.session.len()
    }

    /// Whether stateful state is currently active.
    pub fn is_stateful(&self) -> bool {
        self.last_response_id.is_some()
    }

    /// Consume the wrapper and return the inner session.
    pub fn into_inner(self) -> Session<M> {
        self.session
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::simple::{SimpleMsg, SimpleRole};

    fn make_session() -> Session<SimpleMsg> {
        let dir = std::env::temp_dir().join("sgr_stateful_test");
        let _ = std::fs::remove_dir_all(&dir);
        Session::new(dir.to_str().unwrap(), 60).unwrap()
    }

    #[test]
    fn new_has_no_delta() {
        let session = make_session();
        let stateful = StatefulSession::new(session);
        assert!(stateful.delta().is_none());
        assert!(!stateful.is_stateful());
    }

    #[test]
    fn store_and_delta() {
        let mut session = make_session();
        session.push(SimpleRole::System, "system prompt".into());
        session.push(SimpleRole::User, "hello".into());
        session.push(SimpleRole::Assistant, "hi".into());

        let mut stateful = StatefulSession::new(session);
        stateful.store_response(
            Some("resp_123".into()),
            3,
            vec![("call_1".into(), r#"{"ok":true}"#.into())],
        );

        assert!(stateful.is_stateful());

        let delta = stateful.delta().unwrap();
        assert_eq!(delta.previous_response_id, "resp_123");
        assert_eq!(delta.msg_offset, 3);
        assert_eq!(delta.pending_outputs.len(), 1);

        // Add a new message — delta should include it
        stateful.push(SimpleRole::User, "next question".into());
        let delta2 = stateful.delta().unwrap();
        assert_eq!(delta2.msg_offset, 3); // offset stays at stored count
        assert_eq!(stateful.len(), 4);

        // Clean up
        let dir = std::env::temp_dir().join("sgr_stateful_test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clear_removes_state() {
        let mut session = make_session();
        session.push(SimpleRole::User, "test".into());

        let mut stateful = StatefulSession::new(session);
        stateful.store_response(Some("resp_abc".into()), 1, vec![]);
        assert!(stateful.is_stateful());

        stateful.clear();
        assert!(!stateful.is_stateful());
        assert!(stateful.delta().is_none());

        let dir = std::env::temp_dir().join("sgr_stateful_test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delegate_methods() {
        let session = make_session();
        let mut stateful = StatefulSession::new(session);

        assert!(stateful.is_empty());
        assert_eq!(stateful.len(), 0);

        stateful.push(SimpleRole::User, "hello".into());
        assert!(!stateful.is_empty());
        assert_eq!(stateful.len(), 1);
        assert_eq!(stateful.messages()[0].content(), "hello");

        let dir = std::env::temp_dir().join("sgr_stateful_test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn into_inner() {
        let mut session = make_session();
        session.push(SimpleRole::User, "preserved".into());
        let stateful = StatefulSession::new(session);
        let inner = stateful.into_inner();
        assert_eq!(inner.messages()[0].content(), "preserved");

        let dir = std::env::temp_dir().join("sgr_stateful_test");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn delta_none_when_count_exceeds_messages() {
        let session = make_session();
        let mut stateful = StatefulSession::new(session);
        // Store a count that exceeds actual messages
        stateful.store_response(Some("resp_x".into()), 100, vec![]);
        assert!(stateful.delta().is_none());

        let dir = std::env::temp_dir().join("sgr_stateful_test");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
