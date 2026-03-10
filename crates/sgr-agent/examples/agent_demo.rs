//! Demo: full agent with 15 tools — mirrors rc-cli's 18-tool agent.
//!
//! Omitted: mcp_call (needs server), open_editor (needs GUI), ask_user (needs interactive stdin).
//!
//! Run: cargo run -p sgr-agent --features agent --example agent_demo
//! Custom: cargo run -p sgr-agent --features agent --example agent_demo -- "your prompt"

use sgr_agent::agent_loop::{run_loop, LoopConfig, LoopEvent};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput};
use sgr_agent::agents::sgr::SgrAgent;
use sgr_agent::context::AgentContext;
use sgr_agent::gemini::GeminiClient;
use sgr_agent::registry::ToolRegistry;
use sgr_agent::types::Message;
use sgr_agent::ProviderConfig;
use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn resolve_path(path: &str, cwd: &std::path::Path) -> PathBuf {
    if std::path::Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...\n[truncated, {} bytes total]", &s[..max], s.len())
    } else {
        s.to_string()
    }
}

fn get_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    args.get(key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| ToolError::InvalidArgs(format!("missing '{}'", key)))
}

fn get_opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn run_shell(cmd: &str, cwd: &std::path::Path) -> Result<String, ToolError> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .output()
        .map_err(|e| ToolError::Execution(e.to_string()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() { result.push('\n'); }
        result.push_str("[stderr] ");
        result.push_str(&stderr);
    }
    if result.is_empty() {
        result = format!("[exit code: {}]", output.status.code().unwrap_or(-1));
    }
    Ok(truncate(&result, 8000))
}

// ─── 1. ReadFileTool ─────────────────────────────────────────────────────────

struct ReadFileTool;

#[async_trait::async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str { "read_file" }
    fn description(&self) -> &str { "Read a file from disk. Returns file contents. Supports offset/limit for large files." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to read" },
                "offset": { "type": "integer", "description": "Line offset (0-indexed)" },
                "limit": { "type": "integer", "description": "Max lines to return (default: 1000)" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let path = get_str(&args, "path")?;
        let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(1000) as usize;
        let full_path = resolve_path(path, &ctx.cwd);

        match std::fs::read_to_string(&full_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                let selected: String = lines
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .enumerate()
                    .map(|(i, l)| format!("{:>4}  {}", offset + i + 1, l))
                    .collect::<Vec<_>>()
                    .join("\n");
                let header = format!("[{} — {} lines total, showing {}-{}]\n",
                    full_path.display(), total,
                    offset + 1, (offset + limit).min(total));
                Ok(ToolOutput::text(truncate(&format!("{}{}", header, selected), 8000)))
            }
            Err(e) => Ok(ToolOutput::text(format!("Error: {}: {}", full_path.display(), e))),
        }
    }
}

// ─── 2. WriteFileTool ────────────────────────────────────────────────────────

struct WriteFileTool;

#[async_trait::async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str { "write_file" }
    fn description(&self) -> &str { "Write content to a file (creates or overwrites)." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" },
                "content": { "type": "string", "description": "Full file content to write" }
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let path = get_str(&args, "path")?;
        let content = get_str(&args, "content")?;
        let full_path = resolve_path(path, &ctx.cwd);

        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ToolError::Execution(format!("mkdir: {}", e)))?;
        }
        std::fs::write(&full_path, content)
            .map_err(|e| ToolError::Execution(format!("write: {}", e)))?;

        Ok(ToolOutput::text(format!("Written {} bytes to {}", content.len(), full_path.display())))
    }
}

// ─── 3. EditFileTool ─────────────────────────────────────────────────────────

struct EditFileTool;

#[async_trait::async_trait]
impl Tool for EditFileTool {
    fn name(&self) -> &str { "edit_file" }
    fn description(&self) -> &str { "Edit a file by replacing old_string with new_string. The old_string must match exactly." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path" },
                "old_string": { "type": "string", "description": "Exact string to find and replace" },
                "new_string": { "type": "string", "description": "Replacement string" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let path = get_str(&args, "path")?;
        let old_string = get_str(&args, "old_string")?;
        let new_string = get_str(&args, "new_string")?;
        let full_path = resolve_path(path, &ctx.cwd);

        let content = std::fs::read_to_string(&full_path)
            .map_err(|e| ToolError::Execution(format!("read: {}", e)))?;

        let count = content.matches(old_string).count();
        if count == 0 {
            return Ok(ToolOutput::text(format!("old_string not found in {}", full_path.display())));
        }
        if count > 1 {
            return Ok(ToolOutput::text(format!(
                "old_string found {} times — provide more context to make it unique", count
            )));
        }

        let updated = content.replacen(old_string, new_string, 1);
        std::fs::write(&full_path, &updated)
            .map_err(|e| ToolError::Execution(format!("write: {}", e)))?;

        Ok(ToolOutput::text(format!("Edited {} — replaced 1 occurrence", full_path.display())))
    }
}

// ─── 4. BashTool ─────────────────────────────────────────────────────────────

struct BashTool;

#[async_trait::async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str { "bash" }
    fn description(&self) -> &str { "Run a shell command and return stdout+stderr. For short-lived commands." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to execute" },
                "description": { "type": "string", "description": "What this command does" }
            },
            "required": ["command"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let command = get_str(&args, "command")?;
        let result = run_shell(command, &ctx.cwd)?;
        Ok(ToolOutput::text(result))
    }
}

// ─── 5. BashBgTool ───────────────────────────────────────────────────────────

struct BashBgTool;

#[async_trait::async_trait]
impl Tool for BashBgTool {
    fn name(&self) -> &str { "bash_bg" }
    fn description(&self) -> &str { "Run a long-running command in background (tmux). Use for servers, watchers, etc." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Window name for this background task" },
                "command": { "type": "string", "description": "Long-running command to execute" }
            },
            "required": ["name", "command"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let name = get_str(&args, "name")?;
        let command = get_str(&args, "command")?;

        // Create tmux session if not exists, then new window
        let tmux_cmd = format!(
            "tmux has-session -t rc-bg 2>/dev/null || tmux new-session -d -s rc-bg; \
             tmux new-window -t rc-bg -n '{}' '{}; exec bash'",
            name, command
        );
        let result = run_shell(&tmux_cmd, &ctx.cwd)?;
        Ok(ToolOutput::text(format!("Background task '{}' started.\n{}", name, result)))
    }
}

// ─── 6. SearchCodeTool ───────────────────────────────────────────────────────

struct SearchCodeTool;

#[async_trait::async_trait]
impl Tool for SearchCodeTool {
    fn name(&self) -> &str { "search_code" }
    fn description(&self) -> &str { "Search code: ripgrep content search + fuzzy file name matching. Returns matching lines with context." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search pattern (regex supported)" }
            },
            "required": ["query"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let query = get_str(&args, "query")?;

        // ripgrep content search
        let rg_cmd = format!(
            "rg --line-number --max-count 30 --color never --type-add 'src:*.{{rs,py,ts,js,go,java,c,cpp,h,toml,yaml,json,md}}' -t src '{}' 2>/dev/null | head -50",
            query.replace('\'', "'\\''")
        );
        let content_results = run_shell(&rg_cmd, &ctx.cwd)?;

        // File name search
        let find_cmd = format!(
            "find . -type f -name '*{}*' 2>/dev/null | head -20",
            query.replace('\'', "'\\''")
        );
        let file_results = run_shell(&find_cmd, &ctx.cwd)?;

        let mut result = String::new();
        if !content_results.contains("[exit code:") {
            result.push_str("## Content matches:\n");
            result.push_str(&content_results);
        }
        if !file_results.contains("[exit code:") {
            if !result.is_empty() { result.push_str("\n\n"); }
            result.push_str("## File name matches:\n");
            result.push_str(&file_results);
        }
        if result.is_empty() {
            result = format!("No matches found for '{}'", query);
        }

        Ok(ToolOutput::text(truncate(&result, 8000)))
    }
}

// ─── 7. GitStatusTool ────────────────────────────────────────────────────────

struct GitStatusTool;

#[async_trait::async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str { "git_status" }
    fn description(&self) -> &str { "Show git status: branch, modified files, staged changes." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }
    async fn execute(&self, _args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let branch = run_shell("git branch --show-current", &ctx.cwd).unwrap_or_default();
        let status = run_shell("git status --short", &ctx.cwd)?;
        let log = run_shell("git log --oneline -5", &ctx.cwd).unwrap_or_default();

        let result = format!(
            "Branch: {}\nRecent commits:\n{}\nStatus:\n{}",
            branch.trim(), log.trim(), if status.trim().is_empty() { "  (clean)" } else { status.trim() }
        );
        Ok(ToolOutput::text(result))
    }
}

// ─── 8. GitDiffTool ──────────────────────────────────────────────────────────

struct GitDiffTool;

#[async_trait::async_trait]
impl Tool for GitDiffTool {
    fn name(&self) -> &str { "git_diff" }
    fn description(&self) -> &str { "Show git diff. Optional: specific file, staged changes." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Specific file to diff" },
                "cached": { "type": "boolean", "description": "Show staged changes (--cached)" }
            }
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let mut cmd = String::from("git diff");
        if args.get("cached").and_then(|v| v.as_bool()).unwrap_or(false) {
            cmd.push_str(" --cached");
        }
        if let Some(path) = get_opt_str(&args, "path") {
            cmd.push_str(&format!(" -- '{}'", path));
        }
        let result = run_shell(&cmd, &ctx.cwd)?;
        Ok(ToolOutput::text(if result.trim().is_empty() { "No changes.".into() } else { truncate(&result, 8000) }))
    }
}

// ─── 9. GitAddTool ───────────────────────────────────────────────────────────

struct GitAddTool;

#[async_trait::async_trait]
impl Tool for GitAddTool {
    fn name(&self) -> &str { "git_add" }
    fn description(&self) -> &str { "Stage files for git commit." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Files to stage"
                }
            },
            "required": ["paths"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let paths = args.get("paths")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ToolError::InvalidArgs("missing 'paths' array".into()))?;

        let path_strs: Vec<String> = paths.iter()
            .filter_map(|v| v.as_str().map(|s| format!("'{}'", s)))
            .collect();

        if path_strs.is_empty() {
            return Ok(ToolOutput::text("No paths provided."));
        }

        let cmd = format!("git add {}", path_strs.join(" "));
        let result = run_shell(&cmd, &ctx.cwd)?;
        Ok(ToolOutput::text(format!("Staged {} file(s).\n{}", path_strs.len(), result)))
    }
}

// ─── 10. GitCommitTool ───────────────────────────────────────────────────────

struct GitCommitTool;

#[async_trait::async_trait]
impl Tool for GitCommitTool {
    fn name(&self) -> &str { "git_commit" }
    fn description(&self) -> &str { "Create a git commit with the given message." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "Commit message" }
            },
            "required": ["message"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let message = get_str(&args, "message")?;
        let cmd = format!("git commit -m '{}'", message.replace('\'', "'\\''"));
        let result = run_shell(&cmd, &ctx.cwd)?;
        Ok(ToolOutput::text(result))
    }
}

// ─── 11. ProjectMapTool ──────────────────────────────────────────────────────

struct ProjectMapTool;

#[async_trait::async_trait]
impl Tool for ProjectMapTool {
    fn name(&self) -> &str { "project_map" }
    fn description(&self) -> &str { "Show project structure: directory tree with key files and their roles." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Project directory (defaults to cwd)" }
            }
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let path = get_opt_str(&args, "path").unwrap_or(".");
        let full_path = resolve_path(path, &ctx.cwd);

        // Git-aware tree: tracked files only
        let cmd = format!(
            "cd '{}' && (git ls-files 2>/dev/null || find . -type f -not -path '*/.*' -not -path '*/target/*' -not -path '*/node_modules/*') | \
             head -200 | sort",
            full_path.display()
        );
        let files = run_shell(&cmd, &ctx.cwd)?;

        // Build tree + identify key files
        let key_files_cmd = format!(
            "cd '{}' && for f in Cargo.toml package.json pyproject.toml Makefile README.md CLAUDE.md; do \
               [ -f \"$f\" ] && echo \"$f: $(head -5 \"$f\" | tr '\\n' ' ' | cut -c1-100)\"; \
             done",
            full_path.display()
        );
        let key_info = run_shell(&key_files_cmd, &ctx.cwd).unwrap_or_default();

        let result = format!("## Project: {}\n\n### Files:\n{}\n\n### Key files:\n{}",
            full_path.display(), files, key_info);
        Ok(ToolOutput::text(truncate(&result, 8000)))
    }
}

// ─── 12. DependenciesTool ────────────────────────────────────────────────────

struct DependenciesTool;

#[async_trait::async_trait]
impl Tool for DependenciesTool {
    fn name(&self) -> &str { "dependencies" }
    fn description(&self) -> &str { "Show project dependencies from Cargo.toml / package.json / pyproject.toml." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Manifest file path (auto-detects)" }
            }
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let path = get_opt_str(&args, "path");

        let manifest = if let Some(p) = path {
            resolve_path(p, &ctx.cwd)
        } else {
            // Auto-detect
            let candidates = ["Cargo.toml", "package.json", "pyproject.toml"];
            candidates.iter()
                .map(|c| ctx.cwd.join(c))
                .find(|p| p.exists())
                .ok_or_else(|| ToolError::Execution("No manifest found".into()))?
        };

        let content = std::fs::read_to_string(&manifest)
            .map_err(|e| ToolError::Execution(format!("read: {}", e)))?;

        Ok(ToolOutput::text(format!("[{}]\n{}", manifest.display(), truncate(&content, 6000))))
    }
}

// ─── 13. MemoryTool ──────────────────────────────────────────────────────────

struct MemoryTool;

#[async_trait::async_trait]
impl Tool for MemoryTool {
    fn name(&self) -> &str { "memory" }
    fn description(&self) -> &str { "Save or forget a learned insight. Persists across sessions in MEMORY.jsonl." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["save", "forget"], "description": "save or forget" },
                "category": { "type": "string", "enum": ["decision", "pattern", "preference", "insight", "debug"] },
                "section": { "type": "string", "description": "Topic grouping" },
                "content": { "type": "string", "description": "The actual learning" },
                "context": { "type": "string", "description": "Why this was recorded" },
                "confidence": { "type": "string", "enum": ["confirmed", "tentative"] }
            },
            "required": ["operation", "category", "section", "content"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let operation = get_str(&args, "operation")?;
        let content = get_str(&args, "content")?;
        let section = get_str(&args, "section")?;

        let memory_dir = ctx.cwd.join(".agent-demo");
        std::fs::create_dir_all(&memory_dir).ok();
        let memory_file = memory_dir.join("MEMORY.jsonl");

        match operation {
            "save" => {
                use std::io::Write;
                let entry = serde_json::json!({
                    "category": get_opt_str(&args, "category").unwrap_or("insight"),
                    "section": section,
                    "content": content,
                    "context": get_opt_str(&args, "context"),
                    "confidence": get_opt_str(&args, "confidence").unwrap_or("tentative"),
                    "created": chrono_now(),
                });
                let mut f = std::fs::OpenOptions::new()
                    .create(true).append(true).open(&memory_file)
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                writeln!(f, "{}", entry).map_err(|e| ToolError::Execution(e.to_string()))?;
                Ok(ToolOutput::text(format!("Saved to memory: [{}] {}", section, content)))
            }
            "forget" => {
                // Read, filter out matching content, rewrite
                if let Ok(data) = std::fs::read_to_string(&memory_file) {
                    let filtered: Vec<&str> = data.lines()
                        .filter(|line| !line.contains(content))
                        .collect();
                    std::fs::write(&memory_file, filtered.join("\n"))
                        .map_err(|e| ToolError::Execution(e.to_string()))?;
                    Ok(ToolOutput::text(format!("Forgot memory matching: {}", content)))
                } else {
                    Ok(ToolOutput::text("No memories to forget."))
                }
            }
            _ => Ok(ToolOutput::text(format!("Unknown operation: {}", operation))),
        }
    }
}

fn chrono_now() -> String {
    // Simple ISO timestamp without chrono dep
    let output = Command::new("date").arg("-u").arg("+%Y-%m-%dT%H:%M:%SZ").output();
    output.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

// ─── 14. TaskTool ────────────────────────────────────────────────────────────

struct TaskTool;

#[async_trait::async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str { "task" }
    fn description(&self) -> &str { "Manage tasks: create, list, update status, mark done. Stored in .agent-demo/tasks.json." }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": { "type": "string", "enum": ["create", "list", "update", "done"], "description": "Action" },
                "title": { "type": "string", "description": "Task title (for create)" },
                "task_id": { "type": "integer", "description": "Task ID (for update/done)" },
                "status": { "type": "string", "enum": ["todo", "in_progress", "blocked", "done"] },
                "priority": { "type": "string", "enum": ["low", "medium", "high"] },
                "notes": { "type": "string", "description": "Task description" }
            },
            "required": ["operation"]
        })
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let operation = get_str(&args, "operation")?;
        let task_dir = ctx.cwd.join(".agent-demo");
        std::fs::create_dir_all(&task_dir).ok();
        let task_file = task_dir.join("tasks.json");

        let mut tasks: Vec<Value> = std::fs::read_to_string(&task_file)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        match operation {
            "create" => {
                let title = get_str(&args, "title")?;
                let id = tasks.len() + 1;
                let task = serde_json::json!({
                    "id": id,
                    "title": title,
                    "status": get_opt_str(&args, "status").unwrap_or("todo"),
                    "priority": get_opt_str(&args, "priority").unwrap_or("medium"),
                    "notes": get_opt_str(&args, "notes").unwrap_or(""),
                });
                tasks.push(task);
                std::fs::write(&task_file, serde_json::to_string_pretty(&tasks).unwrap())
                    .map_err(|e| ToolError::Execution(e.to_string()))?;
                Ok(ToolOutput::text(format!("Task #{} created: {}", id, title)))
            }
            "list" => {
                if tasks.is_empty() {
                    return Ok(ToolOutput::text("No tasks."));
                }
                let list: Vec<String> = tasks.iter().map(|t| {
                    format!("#{} [{}] {} ({})",
                        t["id"], t["status"].as_str().unwrap_or("?"),
                        t["title"].as_str().unwrap_or("?"),
                        t["priority"].as_str().unwrap_or("?"))
                }).collect();
                Ok(ToolOutput::text(list.join("\n")))
            }
            "update" | "done" => {
                let task_id = args.get("task_id").and_then(|v| v.as_u64())
                    .ok_or_else(|| ToolError::InvalidArgs("missing task_id".into()))? as usize;
                if let Some(task) = tasks.iter_mut().find(|t| t["id"].as_u64() == Some(task_id as u64)) {
                    if operation == "done" {
                        task["status"] = serde_json::json!("done");
                    } else if let Some(status) = get_opt_str(&args, "status") {
                        task["status"] = serde_json::json!(status);
                    }
                    if let Some(notes) = get_opt_str(&args, "notes") {
                        task["notes"] = serde_json::json!(notes);
                    }
                    std::fs::write(&task_file, serde_json::to_string_pretty(&tasks).unwrap())
                        .map_err(|e| ToolError::Execution(e.to_string()))?;
                    Ok(ToolOutput::text(format!("Task #{} updated.", task_id)))
                } else {
                    Ok(ToolOutput::text(format!("Task #{} not found.", task_id)))
                }
            }
            _ => Ok(ToolOutput::text(format!("Unknown operation: {}", operation))),
        }
    }
}

// ─── 15. AskUserTool ─────────────────────────────────────────────────────────

struct AskUserTool;

#[async_trait::async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str { "ask_user" }
    fn description(&self) -> &str { "Ask the user a clarifying question. Use when the task is ambiguous or you need confirmation before a destructive action." }
    fn is_system(&self) -> bool { true }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "Question to ask the user" }
            },
            "required": ["question"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let question = get_str(&args, "question")?;
        println!("\n  >>> AGENT ASKS: {}", question);
        print!("  >>> YOUR ANSWER: ");
        use std::io::Write;
        std::io::stdout().flush().ok();

        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)
            .map_err(|e| ToolError::Execution(format!("stdin: {}", e)))?;

        Ok(ToolOutput::text(format!("User answered: {}", answer.trim())))
    }
}

// ─── 16. FinishTaskTool ──────────────────────────────────────────────────────

struct FinishTaskTool;

#[async_trait::async_trait]
impl Tool for FinishTaskTool {
    fn name(&self) -> &str { "finish_task" }
    fn description(&self) -> &str { "Call when the user's task is complete. Provide a summary of what was accomplished." }
    fn is_system(&self) -> bool { true }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string", "description": "Summary of what was accomplished" }
            },
            "required": ["summary"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let summary = args.get("summary")
            .and_then(|v| v.as_str())
            .unwrap_or("Done");
        Ok(ToolOutput::done(summary))
    }
}

// ─── Main ────────────────────────────────────────────────────────────────────

const SYSTEM_PROMPT: &str = r#"You are a powerful coding agent with 16 tools. You can read/write/edit files, run shell commands, search code, manage git, track tasks, save memories, and ask the user questions.

RESPONSE FORMAT — always respond with this JSON structure:
{"situation": "your assessment", "task": ["step1", "step2"], "actions": [{"tool_name": "...", ...args}]}

RULES:
- Execute multiple tools per step when they're independent
- Use search_code before editing unfamiliar code
- Use project_map to understand project structure
- Use git_status/git_diff before committing
- Use ask_user when the task is ambiguous or before destructive actions
- When done, call finish_task with a summary
- Be concise, efficient, and precise

AVAILABLE TOOLS:
- read_file: Read files (supports offset/limit)
- write_file: Create or overwrite files
- edit_file: Replace exact strings in files
- bash: Run shell commands
- bash_bg: Run long-running commands in tmux background
- search_code: Ripgrep search + file name matching
- git_status: Branch, modified files, recent commits
- git_diff: Show changes (optional: specific file, --cached)
- git_add: Stage files
- git_commit: Create commit
- project_map: Directory tree + key files
- dependencies: Show project dependencies
- memory: Save/forget learned insights (persists)
- task: Create/list/update/done tasks
- ask_user: Ask the user a clarifying question
- finish_task: Complete the task with summary
"#;

#[tokio::main]
async fn main() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY required");
    let model = std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".into());

    let prompt = std::env::args().nth(1).unwrap_or_else(|| {
        "Explore this project: show the project map, read the main Cargo.toml, then list all crates with a one-line description of each.".into()
    });

    println!("╔══════════════════════════════════════════════════════╗");
    println!("║  sgr-agent framework demo — 16 tools                ║");
    println!("╠══════════════════════════════════════════════════════╣");
    println!("║  Model: {:<44} ║", model);
    println!("║  Prompt: {:<43} ║",
        if prompt.len() > 43 { format!("{}...", &prompt[..40]) } else { prompt.clone() });
    println!("║  Tools: read_file write_file edit_file bash          ║");
    println!("║         bash_bg search_code git_status git_diff      ║");
    println!("║         git_add git_commit project_map dependencies  ║");
    println!("║         memory task ask_user finish_task              ║");
    println!("╚══════════════════════════════════════════════════════╝");
    println!();

    // Build registry — all 16 tools
    let tools = ToolRegistry::new()
        .register(ReadFileTool)
        .register(WriteFileTool)
        .register(EditFileTool)
        .register(BashTool)
        .register(BashBgTool)
        .register(SearchCodeTool)
        .register(GitStatusTool)
        .register(GitDiffTool)
        .register(GitAddTool)
        .register(GitCommitTool)
        .register(ProjectMapTool)
        .register(DependenciesTool)
        .register(MemoryTool)
        .register(TaskTool)
        .register(AskUserTool)
        .register(FinishTaskTool);

    println!("Registered {} tools\n", tools.len());

    // Build agent
    let config = ProviderConfig::gemini(&api_key, &model);
    let client = GeminiClient::new(config);
    let agent = SgrAgent::new(client, SYSTEM_PROMPT);

    // Context
    let mut ctx = AgentContext::new();
    let mut messages = vec![Message::user(&prompt)];
    let loop_config = LoopConfig { max_steps: 15, loop_abort_threshold: 4 };

    // Run!
    let result = run_loop(&agent, &tools, &mut ctx, &mut messages, &loop_config, |event| {
        match event {
            LoopEvent::StepStart { step } => {
                println!("── Step {} ──────────────────────────────────────", step);
            }
            LoopEvent::Decision(d) => {
                if !d.situation.is_empty() {
                    let sit = if d.situation.len() > 120 {
                        format!("{}...", &d.situation[..117])
                    } else {
                        d.situation.clone()
                    };
                    println!("  Situation: {}", sit);
                }
                for tc in &d.tool_calls {
                    let args_str = tc.arguments.to_string();
                    let display = if args_str.len() > 100 {
                        format!("{}...", &args_str[..97])
                    } else {
                        args_str
                    };
                    println!("  -> {}({})", tc.name, display);
                }
            }
            LoopEvent::ToolResult { name, output } => {
                let lines: Vec<&str> = output.lines().collect();
                let preview = if lines.len() > 5 {
                    format!("{}\n       ... ({} more lines)", lines[..5].join("\n       "), lines.len() - 5)
                } else {
                    lines.join("\n       ")
                };
                println!("  <- {} = {}", name, preview);
            }
            LoopEvent::Completed { steps } => {
                println!("\n=== Completed in {} steps ===", steps);
            }
            LoopEvent::LoopDetected { count } => {
                println!("\n!!! Loop detected after {} repetitions !!!", count);
            }
            LoopEvent::Error(e) => {
                println!("\n!!! Error: {} !!!", e);
            }
        }
    }).await;

    match result {
        Ok(steps) => println!("Total steps: {}", steps),
        Err(e) => {
            eprintln!("Agent failed: {}", e);
            std::process::exit(1);
        }
    }
}
