//! Session metadata, listing, search, and import.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use super::traits::EntryType;
use super::format::{make_persisted, parse_entry};
use super::time::{uuid_v7_timestamp, truncate_topic};

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
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
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
        if EntryType::parse(type_str).is_none() { continue; }

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

    let mut file = OpenOptions::new().create(true).truncate(true).write(true).open(&output_path).ok()?;
    for json in &entries {
        let _ = writeln!(file, "{}", json);
    }

    Some(output_path)
}
