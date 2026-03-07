use std::fs::{self, OpenOptions};
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

/// Claude Code compatible session entry (v7 UUIDs, time-sortable).
///
/// Format: `{type, message: {content: [...]}, uuid, sessionId, timestamp, ...}`
/// Reads both this format and legacy `{role, content}` for backward compat.
#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedMessage {
    #[serde(rename = "type")]
    msg_type: String,
    message: serde_json::Value,
    uuid: String,
    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    parent_uuid: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: String,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

fn now_iso() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    // Manual ISO 8601 from epoch (no chrono dependency)
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    let ms = dur.subsec_millis();
    // Days since 1970-01-01
    let (y, mo, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, m, s, ms)
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    // Simplified Gregorian calendar conversion
    let mut y = 1970;
    loop {
        let year_days = if is_leap(y) { 366 } else { 365 };
        if days < year_days { break; }
        days -= year_days;
        y += 1;
    }
    let leap = is_leap(y);
    let months = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut mo = 1;
    for &ml in &months {
        if days < ml { break; }
        days -= ml;
        mo += 1;
    }
    (y, mo, days + 1)
}

fn is_leap(y: u64) -> bool {
    y % 4 == 0 && (y % 100 != 0 || y % 400 == 0)
}

fn make_persisted(msg_type: &str, content: &str, session_id: &str, parent_uuid: Option<&str>) -> PersistedMessage {
    PersistedMessage {
        msg_type: msg_type.to_string(),
        message: serde_json::json!({"content": [{"type": "text", "text": content}]}),
        uuid: uuid::Uuid::now_v7().to_string(),
        parent_uuid: parent_uuid.map(String::from),
        session_id: session_id.to_string(),
        timestamp: now_iso(),
        cwd: std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from)),
    }
}

/// Extract role and content from either new or legacy format.
fn parse_entry(value: &serde_json::Value) -> Option<(String, String)> {
    // Try new format: {type, message: {content: [...]}}
    if let Some(msg_type) = value["type"].as_str() {
        if matches!(msg_type, "user" | "assistant" | "system" | "tool") {
            let content = if let Some(arr) = value["message"]["content"].as_array() {
                arr.iter()
                    .filter_map(|block| {
                        if block["type"].as_str() == Some("text") {
                            block["text"].as_str().map(String::from)
                        } else if block["type"].as_str() == Some("tool_use") {
                            Some(format!("[tool: {}]", block["name"].as_str().unwrap_or("?")))
                        } else if block["type"].as_str() == Some("tool_result") {
                            block["content"].as_str().map(|s| format!("[result: {}]", truncate(s, 200)))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if let Some(s) = value["message"]["content"].as_str() {
                s.to_string()
            } else if let Some(s) = value["message"].as_str() {
                s.to_string()
            } else {
                return None;
            };
            if !content.trim().is_empty() {
                return Some((msg_type.to_string(), content));
            }
        }
        return None;
    }

    // Try legacy format: {role, content}
    if let Some(role) = value["role"].as_str() {
        if let Some(content) = value["content"].as_str() {
            if !content.trim().is_empty() {
                return Some((role.to_string(), content.to_string()));
            }
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max { s } else { &s[..max] }
}

/// Session manager: JSONL persistence, history access, context trimming.
///
/// Uses Claude Code compatible JSONL format with UUID v7 (time-sortable).
pub struct Session<M: AgentMessage> {
    messages: Vec<M>,
    session_file: PathBuf,
    session_id: String,
    last_uuid: Option<String>,
    max_history: usize,
}

impl<M: AgentMessage> Session<M> {
    /// Create a new session with a fresh JSONL file.
    pub fn new(session_dir: &str, max_history: usize) -> Self {
        let _ = std::fs::create_dir_all(session_dir);
        let session_id = uuid::Uuid::now_v7().to_string();
        let session_file = PathBuf::from(format!("{}/{}.jsonl", session_dir, session_id));
        Self {
            messages: Vec::new(),
            session_file,
            session_id,
            last_uuid: None,
            max_history,
        }
    }

    /// Resume from a specific session file.
    pub fn resume(path: &Path, _session_dir: &str, max_history: usize) -> Self {
        let (messages, session_id, last_uuid) = Self::load_file(path);
        Self {
            messages,
            session_file: path.to_path_buf(),
            session_id,
            last_uuid,
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

    pub fn session_id(&self) -> &str {
        &self.session_id
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

    fn persist_last(&mut self) {
        let Some(msg) = self.messages.last() else { return };
        let persisted = make_persisted(
            msg.role().as_str(),
            msg.content(),
            &self.session_id,
            self.last_uuid.as_deref(),
        );
        self.last_uuid = Some(persisted.uuid.clone());
        let Ok(json) = serde_json::to_string(&persisted) else { return };
        let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.session_file) else { return };
        let _ = writeln!(f, "{}", json);
    }

    /// Load a session file, supporting both new and legacy formats.
    /// Returns (messages, session_id, last_uuid).
    fn load_file(path: &Path) -> (Vec<M>, String, Option<String>) {
        let Ok(file) = std::fs::File::open(path) else {
            return (vec![], uuid::Uuid::now_v7().to_string(), None);
        };

        let mut messages = Vec::new();
        let mut session_id = None;
        let mut last_uuid = None;

        for line in BufReader::new(file).lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { continue };

            // Extract session_id and uuid from new format entries
            if let Some(sid) = value["sessionId"].as_str() {
                session_id = Some(sid.to_string());
            }
            if let Some(uid) = value["uuid"].as_str() {
                last_uuid = Some(uid.to_string());
            }

            if let Some((role_str, content)) = parse_entry(&value) {
                if let Some(role) = <M as AgentMessage>::Role::from_str(&role_str) {
                    messages.push(M::new(role, content));
                }
            }
        }

        let sid = session_id.unwrap_or_else(|| {
            // Derive from filename if legacy format
            path.file_stem()
                .and_then(|s| s.to_str())
                .map(String::from)
                .unwrap_or_else(|| uuid::Uuid::now_v7().to_string())
        });

        (messages, sid, last_uuid)
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

/// Metadata about a saved session (lightweight, no full message load).
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// Path to the JSONL file.
    pub path: PathBuf,
    /// Unix timestamp from filename or first entry.
    pub created: u64,
    /// Number of messages (lines in JSONL).
    pub message_count: usize,
    /// First user message — serves as session "topic".
    pub topic: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// Session ID (UUID v7).
    pub session_id: Option<String>,
}

impl SessionMeta {
    /// Extract metadata from a session JSONL file without loading all messages.
    fn from_path(path: &Path) -> Option<Self> {
        let meta = fs::metadata(path).ok()?;
        let filename = path.file_stem()?.to_str()?;

        // Try legacy "session_{ts}" format, otherwise use file modification time
        let created = filename.strip_prefix("session_")
            .and_then(|s| s.parse::<u64>().ok())
            .or_else(|| {
                meta.modified().ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
            })
            .unwrap_or(0);

        let file = fs::File::open(path).ok()?;
        let reader = BufReader::new(file);
        let mut message_count = 0;
        let mut topic = String::new();
        let mut session_id = None;

        for line in reader.lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            message_count += 1;

            if session_id.is_none() {
                session_id = value["sessionId"].as_str().map(String::from);
            }

            if topic.is_empty() {
                if let Some((role, content)) = parse_entry(&value) {
                    if role == "user" {
                        topic = if content.len() > 120 {
                            format!("{}...", &content[..117])
                        } else {
                            content
                        };
                    }
                }
            }
        }

        Some(Self {
            path: path.to_path_buf(),
            created,
            message_count,
            topic,
            size_bytes: meta.len(),
            session_id,
        })
    }
}

/// List all sessions in a directory, sorted by creation time (newest first).
pub fn list_sessions(session_dir: &str) -> Vec<SessionMeta> {
    let Ok(entries) = fs::read_dir(session_dir) else { return vec![] };
    let mut sessions: Vec<SessionMeta> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
        .filter_map(|e| SessionMeta::from_path(&e.path()))
        .collect();
    sessions.sort_by(|a, b| b.created.cmp(&a.created));
    sessions
}

/// Search sessions by fuzzy-matching their topic (first user message).
///
/// Returns matches sorted by score (best first). Requires the `search` feature.
#[cfg(feature = "search")]
pub fn search_sessions(session_dir: &str, query: &str) -> Vec<(u32, SessionMeta)> {
    use nucleo_matcher::{Config, Matcher, Utf32Str};
    use nucleo_matcher::pattern::{Pattern, CaseMatching, Normalization};

    let sessions = list_sessions(session_dir);
    if sessions.is_empty() || query.is_empty() {
        return sessions.into_iter().map(|s| (0, s)).collect();
    }

    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);

    let mut matches: Vec<(u32, SessionMeta)> = sessions
        .into_iter()
        .filter_map(|s| {
            let haystack = Utf32Str::Ascii(s.topic.as_bytes());
            pattern.score(haystack, &mut matcher).map(|score| (score, s))
        })
        .collect();

    matches.sort_by(|a, b| b.0.cmp(&a.0));
    matches
}

/// Import a Claude Code session JSONL into our session directory.
///
/// Since we now use the same format, this mostly copies entries through,
/// filtering to user/assistant/system messages. Legacy Claude sessions
/// with different structure are also handled.
///
/// Returns the output path of the imported session.
pub fn import_claude_session(
    claude_path: &Path,
    output_dir: &str,
) -> Option<PathBuf> {
    let file = fs::File::open(claude_path).ok()?;
    let reader = BufReader::new(file);

    let session_id = uuid::Uuid::now_v7().to_string();
    let mut entries: Vec<String> = Vec::new();
    let mut last_uuid = None;

    for line in reader.lines().map_while(Result::ok) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { continue };

        let msg_type = value["type"].as_str().unwrap_or("");
        if !matches!(msg_type, "user" | "assistant" | "system") { continue; }

        // If it already has the full format, pass through with our session_id
        if value.get("message").is_some() && value.get("uuid").is_some() {
            // Re-serialize with our session_id but preserve the rest
            let mut entry = value.clone();
            entry["sessionId"] = serde_json::Value::String(session_id.clone());
            if let Some(uid) = entry["uuid"].as_str() {
                last_uuid = Some(uid.to_string());
            }
            if let Ok(json) = serde_json::to_string(&entry) {
                entries.push(json);
            }
            continue;
        }

        // Extract content for non-standard entries
        if let Some((role, content)) = parse_entry(&value) {
            let persisted = make_persisted(&role, &content, &session_id, last_uuid.as_deref());
            last_uuid = Some(persisted.uuid.clone());
            if let Ok(json) = serde_json::to_string(&persisted) {
                entries.push(json);
            }
        }
    }

    if entries.is_empty() { return None; }

    fs::create_dir_all(output_dir).ok()?;
    let output_path = Path::new(output_dir).join(format!("{}.jsonl", session_id));

    let mut file = OpenOptions::new().create(true).write(true).open(&output_path).ok()?;
    for json in &entries {
        let _ = writeln!(file, "{}", json);
    }

    Some(output_path)
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
    fn now_iso_produces_valid_timestamp() {
        let ts = now_iso();
        // Should look like 2026-03-07T12:34:56.789Z
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 24); // YYYY-MM-DDTHH:MM:SS.mmmZ
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
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
        let dir = std::env::temp_dir().join("baml_rt_test_persist_v2");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        let sid = session.session_id().to_string();
        session.push(TestRole::User, "hello world".into());
        session.push(TestRole::Assistant, "hi there".into());

        let path = session.session_file().to_path_buf();
        let loaded = Session::<TestMsg>::resume(&path, dir.to_str().unwrap(), 60);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.messages()[0].content(), "hello world");
        assert_eq!(loaded.messages()[1].role(), &TestRole::Assistant);
        assert_eq!(loaded.session_id(), sid);

        // Verify the persisted format is Claude Code compatible
        let raw = std::fs::read_to_string(&path).unwrap();
        let first_line: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(first_line["type"].as_str(), Some("user"));
        assert!(first_line["uuid"].as_str().is_some());
        assert!(first_line["sessionId"].as_str().is_some());
        assert!(first_line["timestamp"].as_str().is_some());
        assert!(first_line["message"]["content"].as_array().is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_parent_uuid_chain() {
        let dir = std::env::temp_dir().join("baml_rt_test_parent_uuid");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "first".into());
        session.push(TestRole::Assistant, "second".into());
        session.push(TestRole::User, "third".into());

        let raw = std::fs::read_to_string(session.session_file()).unwrap();
        let entries: Vec<serde_json::Value> = raw.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        // First message has no parent
        assert!(entries[0]["parentUuid"].is_null());
        // Second message's parent is first message's uuid
        assert_eq!(entries[1]["parentUuid"].as_str(), entries[0]["uuid"].as_str());
        // Third message's parent is second message's uuid
        assert_eq!(entries[2]["parentUuid"].as_str(), entries[1]["uuid"].as_str());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_legacy_format() {
        let dir = std::env::temp_dir().join("baml_rt_test_legacy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Write legacy format manually
        let path = dir.join("session_1234567890.jsonl");
        let legacy = vec![
            r#"{"role":"user","content":"hello legacy"}"#,
            r#"{"role":"assistant","content":"hi from old format"}"#,
        ];
        std::fs::write(&path, legacy.join("\n")).unwrap();

        let loaded = Session::<TestMsg>::resume(&path, dir.to_str().unwrap(), 60);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.messages()[0].content(), "hello legacy");
        assert_eq!(loaded.messages()[1].content(), "hi from old format");
        // Session ID derived from filename for legacy
        assert_eq!(loaded.session_id(), "session_1234567890");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_last_finds_latest() {
        let dir = std::env::temp_dir().join("baml_rt_test_resume_v2");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s1.push(TestRole::User, "first".into());

        // UUID v7 is time-ordered, so a small delay ensures ordering
        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s2.push(TestRole::User, "second".into());

        let resumed = Session::<TestMsg>::resume_last(dir.to_str().unwrap(), 60).unwrap();
        assert_eq!(resumed.messages()[0].content(), "second");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sessions_returns_sorted() {
        let dir = std::env::temp_dir().join("baml_test_list_sessions_v2");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s1.push(TestRole::User, "fix parser bug".into());
        s1.push(TestRole::Assistant, "looking at it".into());

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s2.push(TestRole::User, "add new feature".into());

        let sessions = super::list_sessions(dir.to_str().unwrap());
        assert_eq!(sessions.len(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_meta_extracts_topic() {
        let dir = std::env::temp_dir().join("baml_test_meta_topic_v2");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s.push(TestRole::System, "you are an agent".into());
        s.push(TestRole::User, "deploy to production".into());
        s.push(TestRole::Assistant, "on it".into());

        let meta = super::SessionMeta::from_path(s.session_file()).unwrap();
        assert_eq!(meta.topic, "deploy to production");
        assert_eq!(meta.message_count, 3);
        assert!(meta.size_bytes > 0);
        assert!(meta.session_id.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sessions_empty_dir() {
        let dir = std::env::temp_dir().join("baml_test_list_empty_v2");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);

        let sessions = super::list_sessions(dir.to_str().unwrap());
        assert!(sessions.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(feature = "search")]
    #[test]
    fn search_sessions_fuzzy() {
        let dir = std::env::temp_dir().join("baml_test_search_sessions_v2");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s1.push(TestRole::User, "fix parser bug in baml".into());

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s2.push(TestRole::User, "deploy to production".into());

        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut s3 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s3.push(TestRole::User, "fix loop detection bug".into());

        let results = super::search_sessions(dir.to_str().unwrap(), "fix bug");
        assert!(!results.is_empty());
        let topics: Vec<&str> = results.iter().map(|(_, m)| m.topic.as_str()).collect();
        assert!(topics.iter().any(|t| t.contains("fix")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_claude_session_converts() {
        let dir = std::env::temp_dir().join("baml_test_import_claude_v2");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Create a fake Claude Code session
        let claude_session = dir.join("claude_session.jsonl");
        let lines = vec![
            r#"{"type":"progress","data":{"type":"hook_progress"},"uuid":"a","timestamp":"2026-03-07"}"#,
            r#"{"type":"user","message":{"content":[{"type":"text","text":"fix the parser bug"}]},"uuid":"b","sessionId":"test-sid","timestamp":"2026-03-07"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"Looking at the parser..."},{"type":"tool_use","name":"Read","id":"x"}]},"uuid":"c","sessionId":"test-sid","timestamp":"2026-03-07"}"#,
            r#"{"type":"system","message":{"content":"context info"},"uuid":"d","sessionId":"test-sid","timestamp":"2026-03-07"}"#,
        ];
        std::fs::write(&claude_session, lines.join("\n")).unwrap();

        let out_dir = dir.join("sessions");
        let result = super::import_claude_session(&claude_session, out_dir.to_str().unwrap());
        assert!(result.is_some());

        let output = result.unwrap();
        let content = std::fs::read_to_string(&output).unwrap();
        let entries: Vec<serde_json::Value> = content.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        assert_eq!(entries.len(), 3); // user + assistant + system (progress skipped)
        assert_eq!(entries[0]["type"].as_str(), Some("user"));
        assert_eq!(entries[1]["type"].as_str(), Some("assistant"));
        assert_eq!(entries[2]["type"].as_str(), Some("system"));

        // Verify entries have our sessionId (not the original)
        let sid = entries[0]["sessionId"].as_str().unwrap();
        assert_ne!(sid, "test-sid"); // We assign a new session ID

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn topic_truncated_for_long_messages() {
        let dir = std::env::temp_dir().join("baml_test_long_topic_v2");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        let long_msg = "a".repeat(200);
        s.push(TestRole::User, long_msg);

        let meta = super::SessionMeta::from_path(s.session_file()).unwrap();
        assert!(meta.topic.len() <= 120);
        assert!(meta.topic.ends_with("..."));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uuid_v7_is_time_ordered() {
        let id1 = uuid::Uuid::now_v7().to_string();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let id2 = uuid::Uuid::now_v7().to_string();
        // UUID v7 sorts lexicographically by time
        assert!(id2 > id1, "v7 UUIDs should be time-ordered: {} > {}", id2, id1);
    }
}
