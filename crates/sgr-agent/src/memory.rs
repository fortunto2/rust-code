//! Reusable helpers for SGR agent implementations.
//!
//! Common patterns extracted from va-agent, rc-cli, and other BAML-based agents.

use crate::app_loop::ActionResult;

/// Normalize BAML-generated enum variant names.
///
/// BAML generates Rust enum variants with a `K` prefix (e.g. `Ksystem`, `Kdefault`).
/// This strips it and lowercases for use in IO adapters, signatures, and display.
///
/// ```
/// use sgr_agent::memory::norm;
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
/// use sgr_agent::memory::truncate_json_array;
/// let mut v = serde_json::json!({"items": [1,2,3,4,5,6,7,8,9,10,11,12]});
/// truncate_json_array(&mut v, "items", 3);
/// assert_eq!(v["items"].as_array().unwrap().len(), 4); // 3 items + note
/// ```
pub fn truncate_json_array(value: &mut serde_json::Value, key: &str, max: usize) {
    if let Some(arr) = value.get_mut(key).and_then(|v| v.as_array_mut()) {
        let total = arr.len();
        if total > max {
            arr.truncate(max);
            arr.push(serde_json::json!(format!(
                "... showing {} of {} total",
                max, total
            )));
        }
    }
}

/// Load agent manifesto from standard CWD paths.
///
/// Checks `agent.md` and `.director/agent.md` in the current directory.
/// Returns empty string if none found.
pub fn load_manifesto() -> String {
    load_manifesto_from(std::path::Path::new("."))
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

/// Agent context — layered memory system compatible with Claude Code.
///
/// ## Two loading modes
///
/// ### 1. Agent home dir (`load`)
///
/// Each agent has a home dir (e.g. `.my-agent/`).
/// Inside it, markdown files provide agent-specific context:
///
/// | File | Label | What |
/// |------|-------|------|
/// | `SOUL.md` | Soul | Who the agent is: values, boundaries, tone |
/// | `IDENTITY.md` | Identity | Name, role, stack, domain |
/// | `MANIFESTO.md` | Manifesto | Dev principles, harness engineering |
/// | `RULES.md` | Rules | Coding rules, workflow, constraints |
/// | `MEMORY.md` | Memory | Cross-session learnings, preferences |
/// | `context/*.md` | (filename) | User-extensible extras |
///
/// ### 2. Project dir (`load_project`) — Claude Code compatible
///
/// Loads project-level instructions from standard locations.
/// Prefers `AGENTS.md` (generic) with fallback to `CLAUDE.md` (Claude Code compat).
///
/// | Priority | File | Scope |
/// |----------|------|-------|
/// | 1 | `AGENTS.md` / `CLAUDE.md` / `.claude/CLAUDE.md` | Project instructions (git) |
/// | 2 | `AGENTS.local.md` / `CLAUDE.local.md` | Local instructions (gitignored) |
/// | 3 | `.agents/rules/*.md` / `.claude/rules/*.md` | Rules by topic |
///
/// All files are optional. Missing files are silently skipped.
#[derive(Debug, Default)]
pub struct MemoryContext {
    /// Combined context text for system message injection.
    pub parts: Vec<(String, String)>, // (label, content)
}

impl MemoryContext {
    /// Load context from an agent home directory (SOUL, IDENTITY, MANIFESTO, etc.).
    pub fn load(home_dir: &str) -> Self {
        let dir = std::path::Path::new(home_dir);
        let mut ctx = Self::default();

        const KNOWN_FILES: &[(&str, &str)] = &[
            ("SOUL.md", "Soul"),
            ("IDENTITY.md", "Identity"),
            ("MANIFESTO.md", "Manifesto"),
            ("RULES.md", "Rules"),
            ("MEMORY.md", "Memory (user notes)"),
        ];

        for (filename, label) in KNOWN_FILES {
            let path = dir.join(filename);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if !content.trim().is_empty() {
                    ctx.parts.push((label.to_string(), content));
                }
            }
        }

        // Typed memory from MEMORY.jsonl (agent-written, structured)
        let jsonl_path = dir.join("MEMORY.jsonl");
        if let Some(formatted) = format_memory_jsonl(&jsonl_path) {
            ctx.parts.push(("Memory (learned)".to_string(), formatted));
        }

        // Extra context files from context/ subdir
        load_rules_dir(&dir.join("context"), &mut ctx);

        ctx
    }

    /// Load project-level context (AGENTS.md/CLAUDE.md + rules).
    ///
    /// Claude Code compatible: falls back to CLAUDE.md if AGENTS.md not found.
    pub fn load_project(project_dir: &std::path::Path) -> Self {
        let mut ctx = Self::default();

        // 1. Project instructions: AGENTS.md > CLAUDE.md > .claude/CLAUDE.md
        let project_files: &[(&str, &str)] = &[
            ("AGENTS.md", "Project Instructions"),
            ("CLAUDE.md", "Project Instructions"),
            (".claude/CLAUDE.md", "Project Instructions"),
        ];
        for (filename, label) in project_files {
            let path = project_dir.join(filename);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if !content.trim().is_empty() {
                    let expanded = expand_imports(&content, project_dir, 0);
                    ctx.parts.push((label.to_string(), expanded));
                    break; // first found wins
                }
            }
        }

        // 2. Local instructions: AGENTS.local.md > CLAUDE.local.md
        let local_files: &[(&str, &str)] = &[
            ("AGENTS.local.md", "Local Instructions"),
            ("CLAUDE.local.md", "Local Instructions"),
        ];
        for (filename, label) in local_files {
            let path = project_dir.join(filename);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if !content.trim().is_empty() {
                    let expanded = expand_imports(&content, project_dir, 0);
                    ctx.parts.push((label.to_string(), expanded));
                    break;
                }
            }
        }

        // 3. Rules: .agents/rules/*.md > .claude/rules/*.md
        let rules_dirs = [
            project_dir.join(".agents/rules"),
            project_dir.join(".claude/rules"),
        ];
        for rules_dir in &rules_dirs {
            if rules_dir.is_dir() {
                load_rules_dir(rules_dir, &mut ctx);
                break; // first found dir wins
            }
        }

        ctx
    }

    /// Merge another context into this one (appends parts).
    pub fn merge(&mut self, other: Self) {
        self.parts.extend(other.parts);
    }

    /// Whether any context was found.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }

    /// Combine all parts into a single string for system message injection.
    pub fn to_system_message(&self) -> Option<String> {
        if self.parts.is_empty() {
            return None;
        }
        let sections: Vec<String> = self
            .parts
            .iter()
            .map(|(label, content)| format!("## {}\n{}", label, content.trim()))
            .collect();
        Some(sections.join("\n\n"))
    }

    /// Combine parts with a token budget (chars/4 estimate).
    ///
    /// Priority order (lowest dropped first):
    /// 1. Memory (learned) — tentative entries already GC'd
    /// 2. context/* extras
    /// 3. Manifesto
    /// 4. Rules, Identity, Project/Local Instructions
    /// 5. Soul, Memory (user notes) — never dropped
    pub fn to_system_message_with_budget(&self, max_tokens: usize) -> Option<String> {
        if self.parts.is_empty() {
            return None;
        }

        // Priority: higher = keep longer. Soul and user memory are sacred.
        fn priority(label: &str) -> u8 {
            match label {
                "Soul" => 10,
                "Memory (user notes)" => 9,
                "Identity" => 8,
                "Rules" => 8,
                "Project Instructions" | "Local Instructions" => 7,
                "Memory (learned)" => 6,
                "Manifesto" => 5,
                _ => 3, // context/* extras, rules/*
            }
        }

        let mut indexed: Vec<(u8, &str, &str)> = self
            .parts
            .iter()
            .map(|(label, content)| (priority(label), label.as_str(), content.as_str()))
            .collect();
        // Sort by priority descending — we'll drop from the end
        indexed.sort_by(|a, b| b.0.cmp(&a.0));

        let max_chars = max_tokens * 4;
        let mut total_chars: usize = indexed.iter().map(|(_, l, c)| l.len() + c.len() + 10).sum();

        // Drop lowest priority parts until we fit
        while total_chars > max_chars && !indexed.is_empty() {
            let last = indexed.last().unwrap();
            if last.0 >= 9 {
                break;
            } // never drop Soul or user memory
            total_chars -= last.1.len() + last.2.len() + 10;
            indexed.pop();
        }

        if indexed.is_empty() {
            return None;
        }

        // Restore original order for readability
        indexed.sort_by(|a, b| b.0.cmp(&a.0));
        let sections: Vec<String> = indexed
            .iter()
            .map(|(_, label, content)| format!("## {}\n{}", label, content.trim()))
            .collect();
        Some(sections.join("\n\n"))
    }
}

/// Format MEMORY.jsonl into a readable system message.
///
/// - GC: tentative entries older than 7 days are auto-removed from file
/// - Groups entries by section, shows category and confidence
/// - Limits to last 50 entries to keep context manageable
fn format_memory_jsonl(path: &std::path::Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let mut entries: Vec<serde_json::Value> = content
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    if entries.is_empty() {
        return None;
    }

    // GC: remove tentative entries older than 7 days
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seven_days = 7 * 24 * 3600;
    let before_gc = entries.len();
    entries.retain(|e| {
        let confidence = e["confidence"].as_str().unwrap_or("tentative");
        if confidence == "confirmed" {
            return true;
        }
        let created = e["created"].as_u64().unwrap_or(now_secs);
        now_secs.saturating_sub(created) < seven_days
    });

    // Write back if GC removed anything
    if entries.len() < before_gc {
        let lines: Vec<String> = entries
            .iter()
            .filter_map(|e| serde_json::to_string(e).ok())
            .collect();
        let _ = std::fs::write(path, lines.join("\n") + "\n");
    }

    if entries.is_empty() {
        return None;
    }

    // Keep last 50 entries
    let entries = if entries.len() > 50 {
        &entries[entries.len() - 50..]
    } else {
        &entries[..]
    };
    let mut sections: std::collections::BTreeMap<String, Vec<String>> =
        std::collections::BTreeMap::new();
    for entry in entries {
        let section = entry["section"].as_str().unwrap_or("General").to_string();
        let category = entry["category"].as_str().unwrap_or("note");
        let confidence = entry["confidence"].as_str().unwrap_or("tentative");
        let content = entry["content"].as_str().unwrap_or("");
        let marker = if confidence == "confirmed" {
            "✓"
        } else {
            "?"
        };
        sections
            .entry(section)
            .or_default()
            .push(format!("- [{}|{}] {}", marker, category, content));
    }

    let mut out = String::new();
    for (section, items) in &sections {
        out.push_str(&format!("### {}\n", section));
        for item in items {
            out.push_str(item);
            out.push('\n');
        }
        out.push('\n');
    }
    Some(out)
}

/// Expand `@path/to/file` imports in content (Claude Code compatible).
///
/// Replaces `@relative/path` with the file contents inline.
/// Max depth 5 to prevent cycles. Relative paths resolve from `base_dir`.
fn expand_imports(content: &str, base_dir: &std::path::Path, depth: u8) -> String {
    if depth > 5 {
        return content.to_string();
    }

    let mut result = String::with_capacity(content.len());
    for line in content.lines() {
        let trimmed = line.trim();
        // Match standalone @path or @path in text: "See @README.md for details"
        let expanded = expand_line_imports(trimmed, base_dir, depth);
        result.push_str(&expanded);
        result.push('\n');
    }
    result
}

/// Expand @references within a single line.
fn expand_line_imports(line: &str, base_dir: &std::path::Path, depth: u8) -> String {
    let mut result = String::new();
    let mut rest = line;

    while let Some(at_pos) = rest.find('@') {
        result.push_str(&rest[..at_pos]);
        let after_at = &rest[at_pos + 1..];

        // Extract path: sequence of non-whitespace chars after @
        let path_end = after_at
            .find(|c: char| c.is_whitespace() || c == ',' || c == ')' || c == ']')
            .unwrap_or(after_at.len());
        let ref_path = &after_at[..path_end];

        if ref_path.is_empty() || ref_path.starts_with('{') {
            // Not a file ref (e.g. @{variable})
            result.push('@');
            rest = after_at;
            continue;
        }

        // Resolve path
        let resolved = if ref_path.starts_with('~') {
            let home = std::env::var("HOME").unwrap_or_default();
            std::path::PathBuf::from(ref_path.replacen('~', &home, 1))
        } else {
            base_dir.join(ref_path)
        };

        if resolved.is_file() {
            if let Ok(file_content) = std::fs::read_to_string(&resolved) {
                let parent = resolved.parent().unwrap_or(base_dir);
                let expanded = expand_imports(&file_content, parent, depth + 1);
                result.push_str(expanded.trim());
            } else {
                result.push('@');
                result.push_str(ref_path);
            }
        } else {
            // Not a file — keep as-is (could be @mention or similar)
            result.push('@');
            result.push_str(ref_path);
        }

        rest = &after_at[path_end..];
    }
    result.push_str(rest);
    result
}

/// Load all `*.md` files from a directory, sorted alphabetically.
fn load_rules_dir(dir: &std::path::Path, ctx: &mut MemoryContext) {
    if !dir.is_dir() {
        return;
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            .collect();
        files.sort_by_key(|e| e.file_name());

        for entry in files {
            if let Ok(content) = std::fs::read_to_string(entry.path()) {
                if !content.trim().is_empty() {
                    let label = entry
                        .path()
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("rule")
                        .to_string();
                    ctx.parts.push((label, content));
                }
            }
        }
    }
}

/// Load all `.md` files from a directory (flat, no convention).
///
/// Simpler alternative to [`MemoryContext`] when you just need raw file concat.
pub fn load_context_dir(dir: &str) -> Option<String> {
    let ctx = MemoryContext::load(dir);
    ctx.to_system_message()
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
        let m = load_manifesto_from(std::path::Path::new("/nonexistent"));
        assert!(m.is_empty());
    }

    #[test]
    fn agent_context_loads_known_files() {
        let dir = std::env::temp_dir().join("baml_test_agent_ctx");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SOUL.md"), "Be direct and honest.").unwrap();
        std::fs::write(
            dir.join("IDENTITY.md"),
            "Name: rust-code\nRole: coding agent",
        )
        .unwrap();
        std::fs::write(dir.join("MANIFESTO.md"), "TDD first. Ship > perfect.").unwrap();

        let ctx = MemoryContext::load(dir.to_str().unwrap());
        assert_eq!(ctx.parts.len(), 3);
        assert_eq!(ctx.parts[0].0, "Soul");
        assert_eq!(ctx.parts[1].0, "Identity");
        assert_eq!(ctx.parts[2].0, "Manifesto");

        let msg = ctx.to_system_message().unwrap();
        assert!(msg.contains("Be direct"));
        assert!(msg.contains("rust-code"));
        assert!(msg.contains("TDD first"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_context_loads_extras_from_context_dir() {
        let dir = std::env::temp_dir().join("baml_test_agent_ctx_extras");
        let _ = std::fs::remove_dir_all(&dir);
        let ctx_dir = dir.join("context");
        std::fs::create_dir_all(&ctx_dir).unwrap();
        std::fs::write(dir.join("RULES.md"), "Validate at boundaries.").unwrap();
        std::fs::write(ctx_dir.join("stacks.md"), "Rust + Tokio").unwrap();
        std::fs::write(ctx_dir.join("ignore.txt"), "not loaded").unwrap();

        let ctx = MemoryContext::load(dir.to_str().unwrap());
        assert_eq!(ctx.parts.len(), 2); // RULES + stacks
        assert_eq!(ctx.parts[1].0, "stacks");

        let msg = ctx.to_system_message().unwrap();
        assert!(msg.contains("Validate at boundaries"));
        assert!(msg.contains("Rust + Tokio"));
        assert!(!msg.contains("not loaded"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn agent_context_empty_when_no_dir() {
        let ctx = MemoryContext::load("/nonexistent/path");
        assert!(ctx.is_empty());
        assert!(ctx.to_system_message().is_none());
    }

    #[test]
    fn load_project_prefers_agents_md() {
        let dir = std::env::temp_dir().join("baml_test_project_agents");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "Use pnpm.").unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "Use npm.").unwrap();

        let ctx = MemoryContext::load_project(&dir);
        assert_eq!(ctx.parts.len(), 1);
        assert_eq!(ctx.parts[0].0, "Project Instructions");
        assert!(ctx.parts[0].1.contains("pnpm")); // AGENTS.md wins

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_project_falls_back_to_claude_md() {
        let dir = std::env::temp_dir().join("baml_test_project_claude");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "Build with cargo.").unwrap();

        let ctx = MemoryContext::load_project(&dir);
        assert_eq!(ctx.parts.len(), 1);
        assert!(ctx.parts[0].1.contains("cargo"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_project_loads_local_and_rules() {
        let dir = std::env::temp_dir().join("baml_test_project_full");
        let _ = std::fs::remove_dir_all(&dir);
        let rules_dir = dir.join(".claude/rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "Project X").unwrap();
        std::fs::write(dir.join("CLAUDE.local.md"), "My sandbox URL").unwrap();
        std::fs::write(rules_dir.join("testing.md"), "Run pytest").unwrap();
        std::fs::write(rules_dir.join("style.md"), "Use black").unwrap();

        let ctx = MemoryContext::load_project(&dir);
        assert_eq!(ctx.parts.len(), 4); // CLAUDE + local + 2 rules
        assert_eq!(ctx.parts[0].0, "Project Instructions");
        assert_eq!(ctx.parts[1].0, "Local Instructions");
        // Rules sorted alphabetically
        assert_eq!(ctx.parts[2].0, "style");
        assert_eq!(ctx.parts[3].0, "testing");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_project_agents_rules_over_claude_rules() {
        let dir = std::env::temp_dir().join("baml_test_project_agents_rules");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".agents/rules")).unwrap();
        std::fs::create_dir_all(dir.join(".claude/rules")).unwrap();
        std::fs::write(dir.join(".agents/rules/main.md"), "Agents rule").unwrap();
        std::fs::write(dir.join(".claude/rules/main.md"), "Claude rule").unwrap();

        let ctx = MemoryContext::load_project(&dir);
        assert_eq!(ctx.parts.len(), 1);
        assert!(ctx.parts[0].1.contains("Agents rule")); // .agents/ wins

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_jsonl_loaded_into_context() {
        let dir = std::env::temp_dir().join("baml_test_memory_jsonl");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SOUL.md"), "Be direct.").unwrap();
        // Use fresh timestamp for tentative entry so GC (>7 days) doesn't remove it
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let jsonl = format!(
            r#"{{"category":"decision","section":"Build System","content":"Use cargo, not make","context":"tested both","confidence":"confirmed","created":1772700000}}
{{"category":"pattern","section":"Build System","content":"Always run check before test","context":null,"confidence":"tentative","created":{now}}}
{{"category":"preference","section":"Style","content":"User prefers short commits","context":"observed","confidence":"confirmed","created":1772700200}}
"#
        );
        std::fs::write(dir.join("MEMORY.jsonl"), jsonl).unwrap();

        let ctx = MemoryContext::load(dir.to_str().unwrap());
        // SOUL + Memory (learned)
        assert!(ctx.parts.iter().any(|(l, _)| l == "Memory (learned)"));
        let mem = ctx
            .parts
            .iter()
            .find(|(l, _)| l == "Memory (learned)")
            .unwrap();
        assert!(mem.1.contains("Use cargo, not make"));
        assert!(mem.1.contains("[✓|decision]")); // confirmed
        assert!(mem.1.contains("[?|pattern]")); // tentative
        assert!(mem.1.contains("### Style"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn memory_jsonl_missing_is_ok() {
        let dir = std::env::temp_dir().join("baml_test_no_jsonl");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SOUL.md"), "Be direct.").unwrap();

        let ctx = MemoryContext::load(dir.to_str().unwrap());
        assert!(!ctx.parts.iter().any(|(l, _)| l.contains("learned")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn merge_combines_contexts() {
        let mut a = MemoryContext::default();
        a.parts.push(("Soul".into(), "Be direct.".into()));

        let mut b = MemoryContext::default();
        b.parts.push(("Project".into(), "Use Rust.".into()));

        a.merge(b);
        assert_eq!(a.parts.len(), 2);
        assert_eq!(a.parts[0].0, "Soul");
        assert_eq!(a.parts[1].0, "Project");
    }

    #[test]
    fn gc_removes_old_tentative_entries() {
        let dir = std::env::temp_dir().join("baml_test_memory_gc");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let old = now - 8 * 24 * 3600; // 8 days ago

        let entries = format!(
            "{}\n{}\n{}\n",
            serde_json::json!({"category":"decision","section":"A","content":"confirmed old","confidence":"confirmed","created":old}),
            serde_json::json!({"category":"pattern","section":"B","content":"tentative old","confidence":"tentative","created":old}),
            serde_json::json!({"category":"insight","section":"C","content":"tentative recent","confidence":"tentative","created":now}),
        );
        let path = dir.join("MEMORY.jsonl");
        std::fs::write(&path, &entries).unwrap();

        let formatted = format_memory_jsonl(&path).unwrap();
        // Old tentative should be GC'd
        assert!(!formatted.contains("tentative old"));
        // Confirmed old stays
        assert!(formatted.contains("confirmed old"));
        // Recent tentative stays
        assert!(formatted.contains("tentative recent"));

        // File should be rewritten without old tentative
        let remaining = std::fs::read_to_string(&path).unwrap();
        assert!(!remaining.contains("tentative old"));
        assert_eq!(remaining.lines().count(), 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_expands_file_refs() {
        let dir = std::env::temp_dir().join("baml_test_import");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("README.md"), "# My Project\nThis is the readme.").unwrap();
        std::fs::write(
            dir.join("CLAUDE.md"),
            "See @README.md for overview.\nDo stuff.",
        )
        .unwrap();

        let ctx = MemoryContext::load_project(&dir);
        let msg = ctx.to_system_message().unwrap();
        assert!(msg.contains("This is the readme")); // imported
        assert!(msg.contains("Do stuff")); // original content preserved

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_nonexistent_file_kept_as_is() {
        let dir = std::env::temp_dir().join("baml_test_import_missing");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("CLAUDE.md"), "See @nonexistent.md for info.").unwrap();

        let ctx = MemoryContext::load_project(&dir);
        let msg = ctx.to_system_message().unwrap();
        assert!(msg.contains("@nonexistent.md")); // kept as-is

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_budget_drops_low_priority() {
        let mut ctx = MemoryContext::default();
        ctx.parts.push(("Soul".into(), "Be direct.".into())); // priority 10
        ctx.parts.push(("Manifesto".into(), "x".repeat(10000))); // priority 5, big
        ctx.parts.push(("Identity".into(), "Name: test".into())); // priority 8

        // Budget that fits Soul + Identity but not Manifesto
        let msg = ctx.to_system_message_with_budget(100).unwrap(); // ~400 chars budget
        assert!(msg.contains("Be direct")); // Soul kept
        assert!(msg.contains("Name: test")); // Identity kept
        assert!(!msg.contains("xxxxxxxxx")); // Manifesto dropped
    }

    #[test]
    fn token_budget_never_drops_soul() {
        let mut ctx = MemoryContext::default();
        ctx.parts.push(("Soul".into(), "x".repeat(5000)));

        // Even tiny budget keeps Soul
        let msg = ctx.to_system_message_with_budget(10).unwrap();
        assert!(msg.contains("xxxxx"));
    }
}
