//! Session persistence — Claude Code compatible JSONL format.
//!
//! Modules:
//! - `traits` — MessageRole, AgentMessage, EntryType
//! - `format` — PersistedMessage, parse_entry, make_persisted
//! - `time` — ISO timestamps, UUID v7 extraction, truncation
//! - `store` — Session struct (CRUD, trimming)
//! - `meta` — SessionMeta, list/search/import

pub(crate) mod format;
mod meta;
mod store;
pub(crate) mod time;
pub mod traits;

#[cfg(feature = "search")]
pub use meta::search_sessions;
pub use meta::{import_claude_session, list_sessions, SessionMeta};
pub use store::Session;
pub use traits::{AgentMessage, EntryType, MessageRole};

#[cfg(test)]
pub(crate) mod tests {
    use super::format::{make_persisted, parse_entry};
    use super::time::{now_iso, truncate_str, truncate_topic, uuid_v7_timestamp};
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    pub(crate) enum TestRole {
        System,
        User,
        Assistant,
        Tool,
    }

    impl MessageRole for TestRole {
        fn system() -> Self {
            Self::System
        }
        fn user() -> Self {
            Self::User
        }
        fn assistant() -> Self {
            Self::Assistant
        }
        fn tool() -> Self {
            Self::Tool
        }
        fn as_str(&self) -> &str {
            match self {
                Self::System => "system",
                Self::User => "user",
                Self::Assistant => "assistant",
                Self::Tool => "tool",
            }
        }
        fn parse_role(s: &str) -> Option<Self> {
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
    pub(crate) struct TestMsg {
        pub role: TestRole,
        pub content: String,
    }

    impl AgentMessage for TestMsg {
        type Role = TestRole;
        fn new(role: TestRole, content: String) -> Self {
            Self { role, content }
        }
        fn role(&self) -> &TestRole {
            &self.role
        }
        fn content(&self) -> &str {
            &self.content
        }
    }

    // --- EntryType ---

    #[test]
    fn entry_type_roundtrip() {
        for t in [
            EntryType::User,
            EntryType::Assistant,
            EntryType::System,
            EntryType::Tool,
        ] {
            let json = serde_json::to_string(&t).unwrap();
            let back: EntryType = serde_json::from_str(&json).unwrap();
            assert_eq!(t, back);
        }
    }

    #[test]
    fn entry_type_rejects_invalid() {
        assert!(EntryType::parse("progress").is_none());
        assert!(EntryType::parse("file-history-snapshot").is_none());
        assert!(EntryType::parse("").is_none());
    }

    // --- Format ---

    #[test]
    fn user_message_serialized_as_plain_string() {
        let p = make_persisted(EntryType::User, "hello", "sid", None);
        let json: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert!(json["message"]["content"].is_string());
        assert_eq!(json["message"]["content"].as_str(), Some("hello"));
        assert_eq!(json["message"]["role"].as_str(), Some("user"));
    }

    #[test]
    fn assistant_message_serialized_as_blocks() {
        let p = make_persisted(EntryType::Assistant, "thinking...", "sid", None);
        let json: serde_json::Value = serde_json::to_value(&p).unwrap();
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
            r#"{"type":"progress","data":{"type":"hook_progress"},"uuid":"a"}"#,
        )
        .unwrap();
        assert!(parse_entry(&entry).is_none());
    }

    #[test]
    fn parse_entry_legacy_format() {
        let entry: serde_json::Value =
            serde_json::from_str(r#"{"role":"user","content":"hello legacy"}"#).unwrap();
        let (et, content) = parse_entry(&entry).unwrap();
        assert_eq!(et, EntryType::User);
        assert_eq!(content, "hello legacy");
    }

    // --- Time & truncation ---

    #[test]
    fn now_iso_produces_valid_timestamp() {
        let ts = now_iso();
        assert!(ts.ends_with('Z'));
        assert_eq!(ts.len(), 24);
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn truncate_str_ascii() {
        assert_eq!(truncate_str("hello world", 5), "hello");
        assert_eq!(truncate_str("hi", 10), "hi");
    }

    #[test]
    fn truncate_str_utf8_safe() {
        let s = "ab\u{00e9}cd\u{00fc}ef";
        let t = truncate_str(s, 4);
        assert!(t.len() <= 4);
        assert_eq!(t, "ab\u{00e9}");

        let t2 = truncate_str(s, 3);
        assert!(t2.len() <= 3);
        assert_eq!(t2, "ab");
    }

    #[test]
    fn truncate_str_emoji() {
        let s = "Hello \u{1f30d}\u{1f30d}\u{1f30d}";
        let t = truncate_str(s, 8);
        assert!(t.len() <= 8);
        assert_eq!(t, "Hello ");
    }

    #[test]
    fn truncate_topic_multibyte() {
        let long_multibyte = "\u{00e9}".repeat(200);
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
        assert!(
            id2 > id1,
            "v7 UUIDs should be time-ordered: {} > {}",
            id2,
            id1
        );
    }

    #[test]
    fn uuid_v7_timestamp_extraction() {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let id = uuid::Uuid::now_v7().to_string();
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let ts = uuid_v7_timestamp(&id).unwrap();
        assert!(ts >= before && ts <= after);
    }

    #[test]
    fn uuid_v7_timestamp_invalid() {
        assert!(uuid_v7_timestamp("not-a-uuid").is_none());
        assert!(uuid_v7_timestamp("550e8400-e29b-41d4-a716-446655440000").is_none());
    }

    // --- Session CRUD ---

    #[test]
    fn trim_preserves_system_and_recent() {
        let dir = std::env::temp_dir().join("baml_mod_test_trim");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 10).unwrap();

        session.push(TestRole::System, "sys prompt".into());
        for i in 0..20 {
            let role = if i % 2 == 0 {
                TestRole::User
            } else {
                TestRole::Assistant
            };
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
        let dir = std::env::temp_dir().join("baml_mod_test_noop");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(TestRole::User, "hello".into());
        assert_eq!(session.trim(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_and_reload() {
        let dir = std::env::temp_dir().join("baml_mod_test_persist");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        let sid = session.session_id().to_string();
        session.push(TestRole::User, "hello world".into());
        session.push(TestRole::Assistant, "hi there".into());

        let path = session.session_file().to_path_buf();
        let loaded = Session::<TestMsg>::resume(&path, dir.to_str().unwrap(), 60);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded.messages()[0].content(), "hello world");
        assert_eq!(loaded.messages()[1].role(), &TestRole::Assistant);
        assert_eq!(loaded.session_id(), sid);

        // Verify user = plain string, assistant = blocks
        let raw = std::fs::read_to_string(&path).unwrap();
        let first: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
        assert!(first["message"]["content"].is_string());
        let second: serde_json::Value = serde_json::from_str(raw.lines().nth(1).unwrap()).unwrap();
        assert!(second["message"]["content"].is_array());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_parent_uuid_chain() {
        let dir = std::env::temp_dir().join("baml_mod_test_parent");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(TestRole::User, "first".into());
        session.push(TestRole::Assistant, "second".into());
        session.push(TestRole::User, "third".into());

        let raw = std::fs::read_to_string(session.session_file()).unwrap();
        let entries: Vec<serde_json::Value> = raw
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();

        assert!(entries[0]["parentUuid"].is_null());
        assert_eq!(
            entries[1]["parentUuid"].as_str(),
            entries[0]["uuid"].as_str()
        );
        assert_eq!(
            entries[2]["parentUuid"].as_str(),
            entries[1]["uuid"].as_str()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persist_multibyte_content() {
        let dir = std::env::temp_dir().join("baml_mod_test_multibyte");
        let _ = std::fs::remove_dir_all(&dir);
        let mut session = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        session.push(
            TestRole::User,
            "caf\u{00e9} na\u{00ef}ve r\u{00e9}sum\u{00e9}".into(),
        );
        session.push(TestRole::Assistant, "got it! \u{1f389}".into());

        let path = session.session_file().to_path_buf();
        let loaded = Session::<TestMsg>::resume(&path, dir.to_str().unwrap(), 60);
        assert_eq!(
            loaded.messages()[0].content(),
            "caf\u{00e9} na\u{00ef}ve r\u{00e9}sum\u{00e9}"
        );
        assert_eq!(loaded.messages()[1].content(), "got it! \u{1f389}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_legacy_format() {
        let dir = std::env::temp_dir().join("baml_mod_test_legacy");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let path = dir.join("session_1234567890.jsonl");
        let legacy = [
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
        let dir = std::env::temp_dir().join("baml_mod_test_resume");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        s1.push(TestRole::User, "first".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        s2.push(TestRole::User, "second".into());

        let resumed = Session::<TestMsg>::resume_last(dir.to_str().unwrap(), 60).unwrap();
        assert_eq!(resumed.messages()[0].content(), "second");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- SessionMeta ---

    #[test]
    fn session_meta_extracts_topic() {
        let dir = std::env::temp_dir().join("baml_mod_test_topic");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        s.push(TestRole::System, "you are an agent".into());
        s.push(TestRole::User, "deploy to production".into());

        let meta = SessionMeta::from_path(s.session_file()).unwrap();
        assert_eq!(meta.topic, "deploy to production");
        assert_eq!(meta.message_count, 2);
        assert!(meta.size_bytes > 0);
        assert!(meta.session_id.is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_meta_created_from_uuid_v7() {
        let dir = std::env::temp_dir().join("baml_mod_test_uuid_ts");
        let _ = std::fs::remove_dir_all(&dir);

        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut s = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        s.push(TestRole::User, "test".into());
        let after = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let meta = SessionMeta::from_path(s.session_file()).unwrap();
        assert!(meta.created >= before && meta.created <= after);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sessions_returns_sorted() {
        let dir = std::env::temp_dir().join("baml_mod_test_list");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        s1.push(TestRole::User, "fix parser bug".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        s2.push(TestRole::User, "add new feature".into());

        let sessions = list_sessions(dir.to_str().unwrap());
        assert_eq!(sessions.len(), 2);
        assert!(sessions[0].created >= sessions[1].created);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_sessions_empty_dir() {
        let dir = std::env::temp_dir().join("baml_mod_test_empty");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        assert!(list_sessions(dir.to_str().unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn topic_truncated_for_long_messages() {
        let dir = std::env::temp_dir().join("baml_mod_test_long_topic");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
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
        let dir = std::env::temp_dir().join("baml_mod_test_search");
        let _ = std::fs::remove_dir_all(&dir);

        let mut s1 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        s1.push(TestRole::User, "fix parser bug in baml".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut s2 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
        s2.push(TestRole::User, "deploy to production".into());
        std::thread::sleep(std::time::Duration::from_millis(10));
        let mut s3 = Session::<TestMsg>::new(dir.to_str().unwrap(), 60).unwrap();
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
        let dir = std::env::temp_dir().join("baml_mod_test_import");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let claude_session = dir.join("claude_session.jsonl");
        let lines = [
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
        let entries: Vec<serde_json::Value> = content
            .lines()
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
        let Ok(home) = std::env::var("HOME") else {
            return;
        };
        let claude_dir = std::path::Path::new(&home).join(".claude/projects");
        if !claude_dir.exists() {
            return;
        }

        let Some(project_dir) = std::fs::read_dir(&claude_dir).ok().and_then(|rd| {
            rd.filter_map(|e| e.ok()).find(|e| {
                e.path().is_dir()
                    && std::fs::read_dir(e.path())
                        .ok()
                        .map(|rd2| {
                            rd2.filter_map(|e2| e2.ok())
                                .any(|e2| e2.path().extension().is_some_and(|ext| ext == "jsonl"))
                        })
                        .unwrap_or(false)
            })
        }) else {
            return;
        };

        let smallest = std::fs::read_dir(project_dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "jsonl"))
            .min_by_key(|e| e.metadata().map(|m| m.len()).unwrap_or(u64::MAX));
        let Some(smallest) = smallest else { return };

        let out_dir = std::env::temp_dir().join("baml_mod_test_real");
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
