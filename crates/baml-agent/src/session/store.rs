//! Session struct: JSONL persistence, history, context trimming.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use super::format::{make_persisted, parse_entry};
use super::traits::{AgentMessage, EntryType, MessageRole};

/// Session header metadata — written as the first JSONL line.
///
/// Identifies who created the session, what models are used, etc.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct SessionHeader {
    /// Source client: "sim", "tui", "app", "api"
    pub source: String,
    /// User or operator identifier (login, nickname)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Model used for the agent
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Extra key-value metadata
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub extra: std::collections::HashMap<String, String>,
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
    ///
    /// Creates the session directory if it doesn't exist.
    /// Returns an error if the directory cannot be created.
    pub fn new(session_dir: &str, max_history: usize) -> std::io::Result<Self> {
        Self::new_with_header(session_dir, max_history, None)
    }

    /// Create a new session with an optional header (metadata).
    ///
    /// The header is written as the first JSONL line with `"type": "header"`.
    pub fn new_with_header(
        session_dir: &str,
        max_history: usize,
        header: Option<SessionHeader>,
    ) -> std::io::Result<Self> {
        std::fs::create_dir_all(session_dir)?;
        let session_id = uuid::Uuid::now_v7().to_string();
        let session_file = PathBuf::from(format!("{}/{}.jsonl", session_dir, session_id));

        if let Some(header) = header {
            let header_entry = serde_json::json!({
                "type": "header",
                "sessionId": &session_id,
                "timestamp": super::time::now_iso(),
                "source": header.source,
                "user": header.user,
                "model": header.model,
                "extra": if header.extra.is_empty() { None } else { Some(&header.extra) },
            });
            let mut f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&session_file)?;
            let _ = writeln!(
                f,
                "{}",
                serde_json::to_string(&header_entry).unwrap_or_default()
            );
        }

        Ok(Self {
            messages: Vec::new(),
            session_file,
            session_id,
            last_uuid: None,
            max_history,
        })
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

        let system_msgs: Vec<M> = self
            .messages
            .iter()
            .filter(|m| m.role().is_system())
            .cloned()
            .collect();

        let non_system: Vec<M> = self
            .messages
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
        let Some(msg) = self.messages.last() else {
            return;
        };
        let Some(entry_type) = EntryType::parse(msg.role().as_str()) else {
            return;
        };
        let persisted = make_persisted(
            entry_type,
            msg.content(),
            &self.session_id,
            self.last_uuid.as_deref(),
        );
        self.last_uuid = Some(persisted.uuid.clone());
        let Ok(json) = serde_json::to_string(&persisted) else {
            return;
        };
        let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.session_file)
        else {
            return;
        };
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
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };

            if let Some(sid) = value["sessionId"].as_str() {
                session_id = Some(sid.to_string());
            }
            if let Some(uid) = value["uuid"].as_str() {
                last_uuid = Some(uid.to_string());
            }

            if let Some((entry_type, content)) = parse_entry(&value) {
                messages.push(M::new(
                    entry_type.into_role::<<M as AgentMessage>::Role>(),
                    content,
                ));
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
