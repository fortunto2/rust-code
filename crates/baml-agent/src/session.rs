use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// Role of a message in the agent conversation.
pub trait MessageRole: Clone + PartialEq {
    fn system() -> Self;
    fn user() -> Self;
    fn assistant() -> Self;
    fn tool() -> Self;
    fn as_str(&self) -> &str;
    fn from_str(s: &str) -> Option<Self>;
    fn is_system(&self) -> bool {
        self.as_str() == "system"
    }
}

/// A message in the agent conversation.
pub trait AgentMessage: Clone {
    type Role: MessageRole;
    fn new(role: Self::Role, content: String) -> Self;
    fn role(&self) -> &Self::Role;
    fn content(&self) -> &str;
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedMessage {
    role: String,
    content: String,
}

/// Session manager: JSONL persistence, history access, context trimming.
pub struct Session<M: AgentMessage> {
    messages: Vec<M>,
    session_file: PathBuf,
    max_history: usize,
}

impl<M: AgentMessage> Session<M> {
    /// Create a new session with a fresh JSONL file.
    pub fn new(session_dir: &str, max_history: usize) -> Self {
        let _ = std::fs::create_dir_all(session_dir);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let session_file = PathBuf::from(format!("{}/session_{}.jsonl", session_dir, ts));
        Self {
            messages: Vec::new(),
            session_file,
            max_history,
        }
    }

    /// Resume from a specific session file.
    pub fn resume(path: &Path, _session_dir: &str, max_history: usize) -> Self {
        let messages = Self::load_file(path);
        Self {
            messages,
            session_file: path.to_path_buf(),
            max_history,
        }
    }

    /// Resume the most recent session in the session directory.
    pub fn resume_last(session_dir: &str, max_history: usize) -> Option<Self> {
        let last = Self::find_last_session(session_dir)?;
        Some(Self::resume(&last, session_dir, max_history))
    }

    /// Push a message, persist to JSONL, return ref.
    pub fn push(&mut self, role: <M as AgentMessage>::Role, content: String) -> &M {
        let msg = M::new(role, content);
        self.messages.push(msg);
        self.persist_last();
        self.messages.last().expect("just pushed")
    }

    /// Push a pre-built message.
    pub fn push_msg(&mut self, msg: M) {
        self.messages.push(msg);
        self.persist_last();
    }

    /// Access messages.
    pub fn messages(&self) -> &[M] {
        &self.messages
    }

    /// Mutable access to messages (for external trimming).
    pub fn messages_mut(&mut self) -> &mut Vec<M> {
        &mut self.messages
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn session_file(&self) -> &Path {
        &self.session_file
    }

    /// Trim history to fit context window.
    ///
    /// Preserves system messages and the most recent non-system messages.
    /// Inserts a "[N earlier messages trimmed]" system notice.
    /// Returns the number of trimmed messages (0 if no trimming needed).
    pub fn trim(&mut self) -> usize {
        if self.messages.len() <= self.max_history {
            return 0;
        }

        let system_msgs: Vec<M> = self.messages
            .iter()
            .filter(|m| m.role().is_system())
            .cloned()
            .collect();

        let non_system: Vec<M> = self.messages
            .iter()
            .filter(|m| !m.role().is_system())
            .cloned()
            .collect();

        let keep = self.max_history.saturating_sub(system_msgs.len());
        let skip = non_system.len().saturating_sub(keep);

        if skip == 0 {
            return 0;
        }

        let mut trimmed = system_msgs;
        trimmed.push(M::new(
            <M as AgentMessage>::Role::system(),
            format!("[{} earlier messages trimmed]", skip),
        ));
        trimmed.extend(non_system.into_iter().skip(skip));
        self.messages = trimmed;
        skip
    }

    // --- Private ---

    fn persist_last(&self) {
        let Some(msg) = self.messages.last() else { return };
        let persisted = PersistedMessage {
            role: msg.role().as_str().into(),
            content: msg.content().into(),
        };
        let Ok(json) = serde_json::to_string(&persisted) else { return };
        let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.session_file) else { return };
        let _ = writeln!(f, "{}", json);
    }

    fn load_file(path: &Path) -> Vec<M> {
        let Ok(file) = std::fs::File::open(path) else { return vec![] };
        BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<PersistedMessage>(&line).ok())
            .filter_map(|p| {
                let role = <M as AgentMessage>::Role::from_str(&p.role)?;
                Some(M::new(role, p.content))
            })
            .collect()
    }

    fn find_last_session(dir: &str) -> Option<PathBuf> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        entries.sort_by_key(|e| e.file_name());
        entries.last().map(|e| e.path())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub(crate) enum TestRole { System, User, Assistant, Tool }

    impl MessageRole for TestRole {
        fn system() -> Self { Self::System }
        fn user() -> Self { Self::User }
        fn assistant() -> Self { Self::Assistant }
        fn tool() -> Self { Self::Tool }
        fn as_str(&self) -> &str {
            match self {
                Self::System => "system",
                Self::User => "user",
                Self::Assistant => "assistant",
                Self::Tool => "tool",
            }
        }
        fn from_str(s: &str) -> Option<Self> {
            match s {
                "system" => Some(Self::System),
                "user" => Some(Self::User),
                "assistant" => Some(Self::Assistant),
                "tool" => Some(Self::Tool),
                _ => None,
            }
        }
    }

    #[derive(Clone)]
    pub(crate) struct TestMsg { pub role: TestRole, pub content: String }

    impl AgentMessage for TestMsg {
        type Role = TestRole;
        fn new(role: TestRole, content: String) -> Self { Self { role, content } }
        fn role(&self) -> &TestRole { &self.role }
        fn content(&self) -> &str { &self.content }
    }

    #[test]
    fn trim_preserves_system_and_recent() {
        let dir = std::env::temp_dir().join("baml_rt_test_trim");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 10);

        // 1 system + 20 user/assistant
        session.push(TestRole::System, "sys prompt".into());
        for i in 0..20 {
            let role = if i % 2 == 0 { TestRole::User } else { TestRole::Assistant };
            session.push(role, format!("msg {}", i));
        }
        assert_eq!(session.len(), 21);

        let trimmed = session.trim();
        assert!(trimmed > 0);
        assert!(session.len() <= 12); // 10 max + system + trim notice
        assert_eq!(session.messages()[0].role(), &TestRole::System);
        assert_eq!(session.messages()[0].content(), "sys prompt");
        assert!(session.messages()[1].content().contains("trimmed"));
        assert_eq!(session.messages().last().unwrap().content(), "msg 19");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trim_noop_small_history() {
        let dir = std::env::temp_dir().join("baml_rt_test_noop");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "hello".into());
        assert_eq!(session.trim(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_and_reload() {
        let dir = std::env::temp_dir().join("baml_rt_test_persist");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "hello world".into());
        session.push(TestRole::Assistant, "hi there".into());

        let path = session.session_file().to_path_buf();
        let loaded = Session::<TestMsg>::resume(&path, dir.to_str().unwrap(), 60);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.messages()[0].content(), "hello world");
        assert_eq!(loaded.messages()[1].role(), &TestRole::Assistant);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_last_finds_latest() {
        let dir = std::env::temp_dir().join("baml_rt_test_resume");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s1.push(TestRole::User, "first".into());

        // Ensure different timestamp
        std::thread::sleep(std::time::Duration::from_millis(1100));

        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s2.push(TestRole::User, "second".into());

        let resumed = Session::<TestMsg>::resume_last(dir.to_str().unwrap(), 60).unwrap();
        assert_eq!(resumed.messages()[0].content(), "second");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
