//! Reusable helpers for SGR agent implementations.
//!
//! Common patterns extracted from va-agent, rc-cli, and other BAML-based agents.

use crate::agent_loop::ActionResult;

/// Normalize BAML-generated enum variant names.
///
/// BAML generates Rust enum variants with a `K` prefix (e.g. `Ksystem`, `Kdefault`).
/// This strips it and lowercases for use in IO adapters, signatures, and display.
///
/// ```
/// use baml_agent::helpers::norm;
/// assert_eq!(norm("Ksystem"), "system");
/// assert_eq!(norm("Kdefault"), "default");
/// assert_eq!(norm("already_clean"), "already_clean");
/// ```
pub fn norm(v: &str) -> String {
    if let Some(stripped) = v.strip_prefix("K") {
        stripped.to_ascii_lowercase()
    } else {
        v.to_string()
    }
}

/// Same as [`norm`] but takes an owned String (convenience for `format!("{:?}", variant)`).
pub fn norm_owned(v: String) -> String {
    if let Some(stripped) = v.strip_prefix('K') {
        stripped.to_ascii_lowercase()
    } else {
        v
    }
}

/// Build an `ActionResult` from a JSON value (non-terminal action).
///
/// Common pattern: `execute_xxx() → serde_json::Value → ActionResult`.
pub fn action_result_json(value: &serde_json::Value) -> ActionResult {
    ActionResult {
        output: serde_json::to_string(value).unwrap_or_default(),
        done: false,
    }
}

/// Build an `ActionResult` from a `Result<Value, E>` (non-terminal).
///
/// On error, wraps in `{"error": "..."}`.
pub fn action_result_from<E: std::fmt::Display>(
    result: Result<serde_json::Value, E>,
) -> ActionResult {
    match result {
        Ok(v) => action_result_json(&v),
        Err(e) => action_result_json(&serde_json::json!({"error": e.to_string()})),
    }
}

/// Build a terminal `ActionResult` (signals loop completion).
pub fn action_result_done(summary: &str) -> ActionResult {
    ActionResult {
        output: summary.to_string(),
        done: true,
    }
}

/// Truncate a JSON array in-place, appending a note about total count.
///
/// Useful for keeping context window manageable (segments, beats, etc.).
///
/// ```
/// use baml_agent::helpers::truncate_json_array;
/// let mut v = serde_json::json!({"items": [1,2,3,4,5,6,7,8,9,10,11,12]});
/// truncate_json_array(&mut v, "items", 3);
/// assert_eq!(v["items"].as_array().unwrap().len(), 4); // 3 items + note
/// ```
pub fn truncate_json_array(value: &mut serde_json::Value, key: &str, max: usize) {
    if let Some(arr) = value.get_mut(key).and_then(|v| v.as_array_mut()) {
        let total = arr.len();
        if total > max {
            arr.truncate(max);
            arr.push(serde_json::json!(format!("... showing {} of {} total", max, total)));
        }
    }
}

/// Load agent manifesto from standard CWD paths.
///
/// Checks `agent.md` and `.director/agent.md` in the current directory.
/// Returns empty string if none found.
pub fn load_manifesto() -> String {
    for path in &["agent.md", ".director/agent.md"] {
        if let Ok(content) = std::fs::read_to_string(path) {
            return format!("Project Agent Manifesto:\n---\n{}\n---", content);
        }
    }
    String::new()
}

/// Load agent manifesto from a specific directory.
pub fn load_manifesto_from(dir: &std::path::Path) -> String {
    for name in &["agent.md", ".director/agent.md"] {
        let path = dir.join(name);
        if let Ok(content) = std::fs::read_to_string(&path) {
            return format!("Project Agent Manifesto:\n---\n{}\n---", content);
        }
    }
    String::new()
}

/// Load all `.md` files from a context directory, sorted alphabetically, concatenated.
///
/// Each project can have a context dir (e.g. `.rust-code/context/`, `.va-sessions/context/`)
/// where users place additional instructions as markdown files. These are injected as
/// system messages alongside the BAML prompt.
///
/// Returns `None` if the directory doesn't exist or contains no `.md` files.
pub fn load_context_dir(dir: &str) -> Option<String> {
    let path = std::path::Path::new(dir);
    if !path.is_dir() { return None; }

    let mut entries: Vec<_> = std::fs::read_dir(path).ok()?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut parts = Vec::new();
    for entry in entries {
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            if !content.trim().is_empty() {
                parts.push(content);
            }
        }
    }

    if parts.is_empty() { None } else { Some(parts.join("\n\n---\n\n")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norm_strips_k_prefix() {
        assert_eq!(norm("Ksystem"), "system");
        assert_eq!(norm("Kuser"), "user");
        assert_eq!(norm("Kassistant"), "assistant");
        assert_eq!(norm("Kdefault"), "default");
        assert_eq!(norm("Karchive_master"), "archive_master");
    }

    #[test]
    fn norm_preserves_clean_values() {
        assert_eq!(norm("system"), "system");
        assert_eq!(norm("already_clean"), "already_clean");
        assert_eq!(norm(""), "");
    }

    #[test]
    fn action_result_json_works() {
        let val = serde_json::json!({"ok": true, "count": 5});
        let ar = action_result_json(&val);
        assert!(!ar.done);
        assert!(ar.output.contains("\"ok\":true") || ar.output.contains("\"ok\": true"));
    }

    #[test]
    fn action_result_from_error() {
        let err: Result<serde_json::Value, String> = Err("something broke".into());
        let ar = action_result_from(err);
        assert!(!ar.done);
        assert!(ar.output.contains("something broke"));
    }

    #[test]
    fn action_result_done_sets_flag() {
        let ar = action_result_done("all complete");
        assert!(ar.done);
        assert_eq!(ar.output, "all complete");
    }

    #[test]
    fn truncate_json_array_works() {
        let mut v = serde_json::json!({"items": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]});
        truncate_json_array(&mut v, "items", 3);
        let arr = v["items"].as_array().unwrap();
        assert_eq!(arr.len(), 4); // 3 + note
        assert!(arr[3].as_str().unwrap().contains("12 total"));
    }

    #[test]
    fn truncate_json_array_noop_if_small() {
        let mut v = serde_json::json!({"items": [1, 2, 3]});
        truncate_json_array(&mut v, "items", 10);
        assert_eq!(v["items"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn truncate_json_array_missing_key_noop() {
        let mut v = serde_json::json!({"other": "value"});
        truncate_json_array(&mut v, "items", 3);
        assert!(v.get("items").is_none());
    }

    #[test]
    fn load_manifesto_returns_empty_when_not_found() {
        // In test context, CWD is unlikely to have agent.md
        let m = load_manifesto_from(std::path::Path::new("/nonexistent"));
        assert!(m.is_empty());
    }

    #[test]
    fn load_context_dir_combines_files() {
        let dir = std::env::temp_dir().join("baml_test_ctx_dir");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("01-rules.md"), "# Rules\nBe concise.").unwrap();
        std::fs::write(dir.join("02-persona.md"), "# Persona\nExpert coder.").unwrap();
        std::fs::write(dir.join("ignore.txt"), "not loaded").unwrap();

        let ctx = load_context_dir(dir.to_str().unwrap()).unwrap();
        assert!(ctx.contains("Be concise"));
        assert!(ctx.contains("Expert coder"));
        assert!(!ctx.contains("not loaded"));
        // Files joined with separator
        assert!(ctx.contains("---"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_context_dir_none_when_missing() {
        assert!(load_context_dir("/nonexistent/path").is_none());
    }
}
