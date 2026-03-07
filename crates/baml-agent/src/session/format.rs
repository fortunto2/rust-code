//! Typed session entry format (Claude Code compatible).

use super::time::now_iso;
use super::traits::EntryType;

/// Message body — matches Claude Code's format exactly.
///
/// - user/system: `{role: "user", content: "plain string"}`
/// - assistant: `{role: "assistant", content: [{type: "text", text: "..."}, ...]}`
#[derive(serde::Serialize)]
pub(crate) struct MessageBody {
    pub role: EntryType,
    pub content: MessageContent,
}

/// Content is either a plain string (user/system) or content blocks (assistant/tool).
#[derive(serde::Serialize)]
#[serde(untagged)]
pub(crate) enum MessageContent {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

/// Typed content block for serialization.
#[derive(serde::Serialize)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub(crate) enum ContentBlock {
    Text { text: String },
}

/// Claude Code compatible session entry (v7 UUIDs, time-sortable).
///
/// Serialization-only — reading uses `parse_entry()` on `serde_json::Value`
/// for maximum flexibility with unknown/evolving Claude Code entry types.
#[derive(serde::Serialize)]
pub(crate) struct PersistedMessage {
    #[serde(rename = "type")]
    pub entry_type: EntryType,
    pub message: MessageBody,
    pub uuid: String,
    #[serde(rename = "parentUuid", skip_serializing_if = "Option::is_none")]
    pub parent_uuid: Option<String>,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

pub(crate) fn make_persisted(entry_type: EntryType, content: &str, session_id: &str, parent_uuid: Option<&str>) -> PersistedMessage {
    let message = MessageBody {
        role: entry_type,
        content: match entry_type {
            EntryType::User | EntryType::System => MessageContent::Text(content.to_string()),
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

/// Extract entry type and text content from a JSONL entry.
///
/// Handles three formats:
/// - Claude Code: `{type: "user", message: {role, content: "str" | [{type, text}]}}`
/// - Our format: same as Claude Code (since we now match it)
/// - Legacy: `{role: "user", content: "text"}`
pub(crate) fn parse_entry(value: &serde_json::Value) -> Option<(EntryType, String)> {
    // New format: {type, message: {content: ...}}
    if let Some(type_str) = value["type"].as_str() {
        let entry_type = EntryType::parse(type_str)?;
        let content = extract_content(&value["message"])?;
        if !content.trim().is_empty() {
            return Some((entry_type, content));
        }
        return None;
    }

    // Legacy format: {role, content}
    let entry_type = EntryType::parse(value["role"].as_str()?)?;
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
                        .map(|s| format!("[result: {}]", super::time::truncate_str(s, 200))),
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
