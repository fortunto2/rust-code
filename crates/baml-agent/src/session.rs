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

// ---------------------------------------------------------------------------
// Typed session entry format (Claude Code compatible)
// ---------------------------------------------------------------------------

/// Entry type discriminator — prevents invalid types at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    User,
    Assistant,
    System,
    Tool,
}

impl EntryType {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "system" => Some(Self::System),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }

    fn to_role<R: MessageRole>(&self) -> R {
        match self {
            Self::User => R::user(),
            Self::Assistant => R::assistant(),
            Self::System => R::system(),
            Self::Tool => R::tool(),
        }
    }
}

/// Message body — matches Claude Code's format exactly.
///
/// - user/system: `{role: "user", content: "plain string"}`
/// - assistant: `{role: "assistant", content: [{type: "text", text: "..."}, ...]}`
#[derive(serde::Serialize)]
struct MessageBody {
    role: EntryType,
    content: MessageContent,
}

/// Content is either a plain string (user/system) or content blocks (assistant/tool).
#[derive(serde::Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Typed content block for serialization.
#[derive(serde::Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
enum ContentBlock {
    Text { text: String },
}

/// Claude Code compatible session entry (v7 UUIDs, time-sortable).
///
/// Serialization-only — reading uses `parse_entry()` on `serde_json::Value`
/// for maximum flexibility with unknown/evolving Claude Code entry types.
#[derive(serde::Serialize)]
struct PersistedMessage {
    #[serde(rename = "type")]
    entry_type: EntryType,
    message: MessageBody,
    uuid: String,
    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    parent_uuid: Option<String>,
    #[serde(rename = "sessionId")]
    session_id: String,
    timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
}

// ---------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------

fn now_iso() -> String {
    let dur = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let days = secs / 86400;
    let time_of_day = secs % 86400;
    let h = time_of_day / 3600;
    let m = (time_of_day % 3600) / 60;
    let s = time_of_day % 60;
    let ms = dur.subsec_millis();
    let (y, mo, d) = days_to_ymd(days);
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z", y, mo, d, h, m, s, ms)
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
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

fn make_persisted(entry_type: EntryType, content: &str, session_id: &str, parent_uuid: Option<&str>) -> PersistedMessage {
    let message = MessageBody {
        role: entry_type,
        content: match entry_type {
            // Claude Code uses plain string for user/system
            EntryType::User | EntryType::System => MessageContent::Text(content.to_string()),
            // Claude Code uses content blocks array for assistant/tool
            EntryType::Assistant | EntryType::Tool => MessageContent::Blocks(vec![
                ContentBlock::Text { text: content.to_string() },
            ]),
        },
    };
    PersistedMessage {
        entry_type,
        message,
        uuid: uuid::Uuid::now_v7().to_string(),
        parent_uuid: parent_uuid.map(String::from),
        session_id: session_id.to_string(),
        timestamp: now_iso(),
        cwd: std::env::current_dir().ok().and_then(|p| p.to_str().map(String::from)),
    }
}

// ---------------------------------------------------------------------------
// Parsing (reads any format: Claude Code, our format, legacy)
// ---------------------------------------------------------------------------

/// Extract entry type and text content from a JSONL entry.
///
/// Handles three formats:
/// - Claude Code: `{type: "user", message: {role, content: "str" | [{type, text}]}}`
/// - Our format: same as Claude Code (since we now match it)
/// - Legacy: `{role: "user", content: "text"}`
fn parse_entry(value: &serde_json::Value) -> Option<(EntryType, String)> {
    // New format: {type, message: {content: ...}}
    if let Some(type_str) = value["type"].as_str() {
        let entry_type = EntryType::from_str(type_str)?;
        let content = extract_content(&value["message"])?;
        if !content.trim().is_empty() {
            return Some((entry_type, content));
        }
        return None;
    }

    // Legacy format: {role, content}
    let entry_type = EntryType::from_str(value["role"].as_str()?)?;
    let content = value["content"].as_str()?;
    if !content.trim().is_empty() {
        return Some((entry_type, content.to_string()));
    }
    None
}

/// Extract text content from a message body.
///
/// Handles: plain string, content block array, or direct string.
fn extract_content(message: &serde_json::Value) -> Option<String> {
    let content = &message["content"];

    // Array of content blocks (assistant format)
    if let Some(arr) = content.as_array() {
        let parts: Vec<String> = arr.iter()
            .filter_map(|block| {
                match block["type"].as_str()? {
                    "text" => block["text"].as_str().map(String::from),
                    "tool_use" => Some(format!("[tool: {}]", block["name"].as_str().unwrap_or("?"))),
                    "tool_result" => block["content"].as_str()
                        .map(|s| format!("[result: {}]", truncate_str(s, 200))),
                    _ => None, // skip thinking, etc.
                }
            })
            .collect();
        return if parts.is_empty() { None } else { Some(parts.join("\n")) };
    }

    // Plain string content (user format)
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }

    // Direct string message (rare)
    if let Some(s) = message.as_str() {
        return Some(s.to_string());
    }

    None
}

/// UTF-8 safe string truncation (never panics on multibyte chars).
fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// Extract unix timestamp (seconds) from a UUID v7 string.
fn uuid_v7_timestamp(uuid_str: &str) -> Option<u64> {
    let uuid = uuid::Uuid::parse_str(uuid_str).ok()?;
    let (secs, _nanos) = uuid.get_timestamp()?.to_unix();
    Some(secs)
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

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
        let Some(entry_type) = EntryType::from_str(msg.role().as_str()) else { return };
        let persisted = make_persisted(
            entry_type,
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

            if let Some(sid) = value["sessionId"].as_str() {
                session_id = Some(sid.to_string());
            }
            if let Some(uid) = value["uuid"].as_str() {
                last_uuid = Some(uid.to_string());
            }

            if let Some((entry_type, content)) = parse_entry(&value) {
                messages.push(M::new(entry_type.to_role::<<M as AgentMessage>::Role>(), content));
            }
        }

        let sid = session_id.unwrap_or_else(|| {
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

// ---------------------------------------------------------------------------
// SessionMeta
// ---------------------------------------------------------------------------

/// Metadata about a saved session (lightweight, no full message load).
#[derive(Debug, Clone)]
pub struct SessionMeta {
    /// Path to the JSONL file.
    pub path: PathBuf,
    /// Unix timestamp (seconds) — extracted from UUID v7, filename, or file mtime.
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

        let file = fs::File::open(path).ok()?;
        let reader = BufReader::new(file);
        let mut message_count = 0;
        let mut topic = String::new();
        let mut session_id = None;
        let mut first_uuid = None;

        for line in reader.lines().map_while(Result::ok) {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
            message_count += 1;

            if session_id.is_none() {
                session_id = value["sessionId"].as_str().map(String::from);
            }
            if first_uuid.is_none() {
                first_uuid = value["uuid"].as_str().map(String::from);
            }

            if topic.is_empty() {
                if let Some((EntryType::User, content)) = parse_entry(&value) {
                    topic = truncate_topic(&content);
                }
            }
        }

        // Determine creation time: UUID v7 timestamp > filename > file mtime
        let created = first_uuid.as_deref()
            .and_then(uuid_v7_timestamp)
            .or_else(|| filename.strip_prefix("session_")?.parse::<u64>().ok())
            .or_else(|| {
                meta.modified().ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
            })
            .unwrap_or(0);

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

/// Truncate topic for display, respecting UTF-8 boundaries.
fn truncate_topic(s: &str) -> String {
    if s.len() <= 120 {
        s.to_string()
    } else {
        let truncated = truncate_str(s, 117);
        format!("{}...", truncated)
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

        let type_str = value["type"].as_str().unwrap_or("");
        if EntryType::from_str(type_str).is_none() { continue; }

        // If it already has the full format, pass through with our session_id
        if value.get("message").is_some() && value.get("uuid").is_some() {
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
        if let Some((entry_type, content)) = parse_entry(&value) {
            let persisted = make_persisted(entry_type, &content, &session_id, last_uuid.as_deref());
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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

    // --- Type safety tests ---

    #[test]
    fn entry_type_roundtrip() {
        for t in [EntryType::User, EntryType::Assistant, EntryType::System, EntryType::Tool] {
            let json = serde_json::to_string(&t).unwrap();
            let back: EntryType = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn entry_type_rejects_invalid() {
        assert!(EntryType::from_str("progress").is_none());
        assert!(EntryType::from_str("file-history-snapshot").is_none());
        assert!(EntryType::from_str("").is_none());
    }

    #[test]
    fn user_message_serialized_as_plain_string() {
        let p = make_persisted(EntryType::User, "hello", "sid", None);
        let json: serde_json::Value = serde_json::to_value(&p).unwrap();
        // User content should be a plain string, not blocks array
        assert!(json["message"]["content"].is_string());
        assert_eq!(json["message"]["content"].as_str(), Some("hello"));
        assert_eq!(json["message"]["role"].as_str(), Some("user"));
    }

    #[test]
    fn assistant_message_serialized_as_blocks() {
        let p = make_persisted(EntryType::Assistant, "thinking...", "sid", None);
        let json: serde_json::Value = serde_json::to_value(&p).unwrap();
        // Assistant content should be blocks array
        let blocks = json["message"]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"].as_str(), Some("text"));
        assert_eq!(blocks[0]["text"].as_str(), Some("thinking..."));
    }

    #[test]
    fn system_message_serialized_as_plain_string() {
        let p = make_persisted(EntryType::System, "you are an agent", "sid", None);
        let json: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert!(json["message"]["content"].is_string());
    }

    // --- UTF-8 safety ---

    #[test]
    fn truncate_str_ascii() {
        assert_eq!(truncate_str("hello world", 5), "hello");
        assert_eq!(truncate_str("hi", 10), "hi");
    }

    #[test]
    fn truncate_str_utf8_safe() {
        // Multibyte chars: each CJK/Cyrillic char is 2+ bytes in UTF-8
        let s = "ab\u{00e9}cd\u{00fc}ef"; // 10 bytes (a,b,é,c,d,ü,e,f = 6 ASCII + 2×2-byte)
        let t = truncate_str(s, 4);
        assert!(t.len() <= 4);
        assert_eq!(t, "ab\u{00e9}"); // 'a'(1) + 'b'(1) + 'é'(2) = 4 bytes

        // Odd limit must not split a 2-byte char
        let t2 = truncate_str(s, 3);
        assert!(t2.len() <= 3);
        assert_eq!(t2, "ab"); // 'é' at byte 2 is 2 bytes, doesn't fit in 3
    }

    #[test]
    fn truncate_str_emoji() {
        let s = "Hello 🌍🌍🌍";
        let t = truncate_str(s, 8);
        // "Hello " is 6 bytes, 🌍 is 4 bytes → doesn't fit at 8
        assert!(t.len() <= 8);
        assert_eq!(t, "Hello "); // 6 bytes, emoji doesn't fit
    }

    #[test]
    fn truncate_topic_multibyte() {
        let long_multibyte = "\u{00e9}".repeat(200); // 400 bytes (2-byte chars)
        let topic = truncate_topic(&long_multibyte);
        assert!(topic.ends_with("..."));
        assert!(topic.len() <= 120);
    }

    // --- UUID v7 ---

    #[test]
    fn uuid_v7_is_time_ordered() {
        let id1 = uuid::Uuid::now_v7().to_string();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let id2 = uuid::Uuid::now_v7().to_string();
        assert!(id2 > id1, "v7 UUIDs should be time-ordered: {} > {}", id2, id1);
    }

    #[test]
    fn uuid_v7_timestamp_extraction() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let id = uuid::Uuid::now_v7().to_string();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let ts = uuid_v7_timestamp(&id).unwrap();
        assert!(ts >= before && ts <= after);
    }

    #[test]
    fn uuid_v7_timestamp_invalid() {
        assert!(uuid_v7_timestamp("not-a-uuid").is_none());
        // UUID v4 has no timestamp
        assert!(uuid_v7_timestamp("550e8400-e29b-41d4-a716-446655440000").is_none());
    }

    // --- Timestamps ---

    #[test]
    fn now_iso_produces_valid_timestamp() {
        let ts = now_iso();
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 24);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    // --- parse_entry ---

    #[test]
    fn parse_entry_claude_code_user_format() {
        let entry: serde_json::Value = serde_json::from_str(
            r#"{"type":"user","message":{"role":"user","content":"fix the bug"},"uuid":"abc","sessionId":"s1","timestamp":"2026-03-07T10:00:00.000Z"}"#
        ).unwrap();
        let (et, content) = parse_entry(&entry).unwrap();
        assert_eq!(et, EntryType::User);
        assert_eq!(content, "fix the bug");
    }

    #[test]
    fn parse_entry_claude_code_assistant_format() {
        let entry: serde_json::Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking at it..."},{"type":"tool_use","name":"Read","id":"x","input":{}}]},"uuid":"def","sessionId":"s1","timestamp":"2026-03-07T10:00:01.000Z"}"#
        ).unwrap();
        let (et, content) = parse_entry(&entry).unwrap();
        assert_eq!(et, EntryType::Assistant);
        assert!(content.contains("Looking at it..."));
        assert!(content.contains("[tool: Read]"));
    }

    #[test]
    fn parse_entry_skips_thinking_only() {
        let entry: serde_json::Value = serde_json::from_str(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm..."}]},"uuid":"ghi","sessionId":"s1","timestamp":"2026-03-07T10:00:02.000Z"}"#
        ).unwrap();
        assert!(parse_entry(&entry).is_none());
    }

    #[test]
    fn parse_entry_skips_progress() {
        let entry: serde_json::Value = serde_json::from_str(
            r#"{"type":"progress","data":{"type":"hook_progress"},"uuid":"a"}"#
        ).unwrap();
        assert!(parse_entry(&entry).is_none());
    }

    #[test]
    fn parse_entry_legacy_format() {
        let entry: serde_json::Value = serde_json::from_str(
            r#"{"role":"user","content":"hello legacy"}"#
        ).unwrap();
        let (et, content) = parse_entry(&entry).unwrap();
        assert_eq!(et, EntryType::User);
        assert_eq!(content, "hello legacy");
    }

    // --- Session CRUD ---

    #[test]
    fn trim_preserves_system_and_recent() {
        let dir = std::env::temp_dir().join("baml_rt_test_trim3");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 10);

        session.push(TestRole::System, "sys prompt".into());
        for i in 0..20 {
            let role = if i % 2 == 0 { TestRole::User } else { TestRole::Assistant };
            session.push(role, format!("msg {}", i));
        }
        assert_eq!(session.len(), 21);

        let trimmed = session.trim();
        assert!(trimmed > 0);
        assert!(session.len() <= 12);
        assert_eq!(session.messages()[0].role(), &TestRole::System);
        assert!(session.messages()[1].content().contains("trimmed"));
        assert_eq!(session.messages().last().unwrap().content(), "msg 19");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trim_noop_small_history() {
        let dir = std::env::temp_dir().join("baml_rt_test_noop3");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "hello".into());
        assert_eq!(session.trim(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_and_reload() {
        let dir = std::env::temp_dir().join("baml_rt_test_persist_v3");
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

        // Verify user message uses plain string format
        let raw = std::fs::read_to_string(&path).unwrap();
        let first: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert_eq!(first["type"].as_str(), Some("user"));
        assert!(first["message"]["content"].is_string()); // plain string, not blocks

        // Verify assistant message uses blocks format
        let second: serde_json::Value = serde_json::from_str(raw.lines().nth(1).unwrap()).unwrap();
        assert_eq!(second["type"].as_str(), Some("assistant"));
        assert!(second["message"]["content"].is_array()); // blocks array

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_parent_uuid_chain() {
        let dir = std::env::temp_dir().join("baml_rt_test_parent_uuid3");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "first".into());
        session.push(TestRole::Assistant, "second".into());
        session.push(TestRole::User, "third".into());

        let raw = std::fs::read_to_string(session.session_file()).unwrap();
        let entries: Vec<serde_json::Value> = raw.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        assert!(entries[0]["parentUuid"].is_null());
        assert_eq!(entries[1]["parentUuid"].as_str(), entries[0]["uuid"].as_str());
        assert_eq!(entries[2]["parentUuid"].as_str(), entries[1]["uuid"].as_str());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_multibyte_content() {
        let dir = std::env::temp_dir().join("baml_rt_test_multibyte");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        session.push(TestRole::User, "caf\u{00e9} na\u{00ef}ve r\u{00e9}sum\u{00e9}".into());
        session.push(TestRole::Assistant, "got it! \u{1f389}".into());

        let path = session.session_file().to_path_buf();
        let loaded = Session::<TestMsg>::resume(&path, dir.to_str().unwrap(), 60);
        assert_eq!(loaded.messages()[0].content(), "caf\u{00e9} na\u{00ef}ve r\u{00e9}sum\u{00e9}");
        assert_eq!(loaded.messages()[1].content(), "got it! \u{1f389}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_legacy_format() {
        let dir = std::env::temp_dir().join("baml_rt_test_legacy3");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = dir.join("session_1234567890.jsonl");
        let legacy = vec![
            r#"{"role":"user","content":"hello legacy"}"#,
            r#"{"role":"assistant","content":"hi from old format"}"#,
        ];
        std::fs::write(&path, legacy.join("\n")).unwrap();

        let loaded = Session::<TestMsg>::resume(&path, dir.to_str().unwrap(), 60);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.messages()[0].content(), "hello legacy");
        assert_eq!(loaded.session_id(), "session_1234567890");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_last_finds_latest() {
        let dir = std::env::temp_dir().join("baml_rt_test_resume_v3");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s1.push(TestRole::User, "first".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s2.push(TestRole::User, "second".into());

        let resumed = Session::<TestMsg>::resume_last(dir.to_str().unwrap(), 60).unwrap();
        assert_eq!(resumed.messages()[0].content(), "second");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- SessionMeta ---

    #[test]
    fn session_meta_extracts_topic() {
        let dir = std::env::temp_dir().join("baml_test_meta_topic_v3");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s.push(TestRole::System, "you are an agent".into());
        s.push(TestRole::User, "deploy to production".into());

        let meta = SessionMeta::from_path(s.session_file()).unwrap();
        assert_eq!(meta.topic, "deploy to production");
        assert_eq!(meta.message_count, 2);
        assert!(meta.size_bytes > 0);
        assert!(meta.session_id.is_some());
        // created should be extracted from UUID v7 timestamp
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        assert!(meta.created > 0 && meta.created <= now);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_meta_created_from_uuid_v7() {
        let dir = std::env::temp_dir().join("baml_test_meta_uuid_ts");
        let _ = std::fs::remove_dir_all(&dir);

        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mut s = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s.push(TestRole::User, "test".into());
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

        let meta = SessionMeta::from_path(s.session_file()).unwrap();
        assert!(meta.created >= before && meta.created <= after,
            "created {} should be between {} and {}", meta.created, before, after);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sessions_returns_sorted() {
        let dir = std::env::temp_dir().join("baml_test_list_v3");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s1.push(TestRole::User, "fix parser bug".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s2.push(TestRole::User, "add new feature".into());

        let sessions = list_sessions(dir.to_str().unwrap());
        assert_eq!(sessions.len(), 2);
        assert!(sessions[0].created >= sessions[1].created);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sessions_empty_dir() {
        let dir = std::env::temp_dir().join("baml_test_empty_v3");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        assert!(list_sessions(dir.to_str().unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn topic_truncated_for_long_messages() {
        let dir = std::env::temp_dir().join("baml_test_long_topic_v3");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s.push(TestRole::User, "a".repeat(200));

        let meta = SessionMeta::from_path(s.session_file()).unwrap();
        assert!(meta.topic.len() <= 120);
        assert!(meta.topic.ends_with("..."));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Search ---

    #[cfg(feature = "search")]
    #[test]
    fn search_sessions_fuzzy() {
        let dir = std::env::temp_dir().join("baml_test_search_v3");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s1.push(TestRole::User, "fix parser bug in baml".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s2.push(TestRole::User, "deploy to production".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut s3 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60);
        s3.push(TestRole::User, "fix loop detection bug".into());

        let results = search_sessions(dir.to_str().unwrap(), "fix bug");
        assert!(!results.is_empty());
        let topics: Vec<&str> = results.iter().map(|(_, m)| m.topic.as_str()).collect();
        assert!(topics.iter().any(|t| t.contains("fix")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Import ---

    #[test]
    fn import_claude_session_converts() {
        let dir = std::env::temp_dir().join("baml_test_import_v3");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let claude_session = dir.join("claude_session.jsonl");
        let lines = vec![
            r#"{"type":"progress","data":{"type":"hook_progress"},"uuid":"a","timestamp":"2026-03-07"}"#,
            r#"{"type":"user","message":{"role":"user","content":"fix the parser bug"},"uuid":"b","sessionId":"test-sid","timestamp":"2026-03-07"}"#,
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Looking at the parser..."},{"type":"tool_use","name":"Read","id":"x","input":{}}]},"uuid":"c","sessionId":"test-sid","timestamp":"2026-03-07"}"#,
            r#"{"type":"system","message":{"content":"context info"},"uuid":"d","sessionId":"test-sid","timestamp":"2026-03-07"}"#,
        ];
        std::fs::write(&claude_session, lines.join("\n")).unwrap();

        let out_dir = dir.join("sessions");
        let result = import_claude_session(&claude_session, out_dir.to_str().unwrap());
        assert!(result.is_some());

        let output = result.unwrap();
        let content = std::fs::read_to_string(&output).unwrap();
        let entries: Vec<serde_json::Value> = content.lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["type"].as_str(), Some("user"));
        assert_eq!(entries[1]["type"].as_str(), Some("assistant"));
        assert_eq!(entries[2]["type"].as_str(), Some("system"));

        let sid = entries[0]["sessionId"].as_str().unwrap();
        assert_ne!(sid, "test-sid");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Integration ---

    #[test]
    fn load_real_claude_session() {
        // Integration test: import a real Claude Code session if ~/.claude/ exists.
        // Skips gracefully on CI or machines without Claude Code.
        let Ok(home) = std::env::var("HOME") else { return };
        let claude_dir = std::path::Path::new(&home).join(".claude/projects");
        if !claude_dir.exists() { return; }

        // Find any project dir with .jsonl sessions
        let Some(project_dir) = std::fs::read_dir(&claude_dir).ok()
            .and_then(|rd| rd.filter_map(|e| e.ok())
                .find(|e| e.path().is_dir() && std::fs::read_dir(e.path()).ok()
                    .map(|rd2| rd2.filter_map(|e2| e2.ok())
                        .any(|e2| e2.path().extension().is_some_and(|ext| ext == "jsonl")))
                    .unwrap_or(false)))
        else { return };

        let smallest = std::fs::read_dir(project_dir.path()).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .min_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(u64::MAX));
        let Some(smallest) = smallest else { return };

        let out_dir = std::env::temp_dir().join("baml_test_real_v3");
        let _ = std::fs::remove_dir_all(&out_dir);

        let result = import_claude_session(&smallest.path(), out_dir.to_str().unwrap());
        if let Some(output) = result {
            let content = std::fs::read_to_string(&output).unwrap();
            assert!(content.lines().count() > 0);
            for line in content.lines() {
                let v: serde_json::Value = serde_json::from_str(line).unwrap();
                assert!(v["type"].as_str().is_some());
                assert!(v["sessionId"].as_str().is_some());
            }
        }

        let _ = std::fs::remove_dir_all(&out_dir);
    }
}
