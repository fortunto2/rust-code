# sgr-agent-tools

[![Crates.io](https://img.shields.io/crates/v/sgr-agent-tools)](https://crates.io/crates/sgr-agent-tools)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

14 reusable file-system tools + 2 backends for [sgr-agent](https://crates.io/crates/sgr-agent) AI agents.

All tools are generic over `FileBackend` — implement it once for your runtime and get battle-tested tools out of the box.

## Tools

### Core (always available)

| # | Tool | Description |
|---|------|-------------|
| 1 | `ReadTool` | Read file with trust metadata + indentation-aware mode |
| 2 | `WriteTool` | Write file with JSON auto-repair |
| 3 | `DeleteTool` | Delete files (single or batch) |
| 4 | `SearchTool` | Smart search: query expansion, fuzzy regex, Levenshtein, auto-expand |
| 5 | `ListTool` | List directory |
| 6 | `TreeTool` | Directory tree |
| 7 | `ReadAllTool` | Batch read all files in directory |
| 8 | `MkDirTool` | Create directory (deferred) |
| 9 | `MoveTool` | Move/rename file (deferred) |
| 10 | `FindTool` | Find by name pattern (deferred) |

### Optional (feature-gated)

| # | Tool | Feature | Description |
|---|------|---------|-------------|
| 11 | `EvalTool` | `eval` | JavaScript via Boa engine |
| 12 | `ShellTool` | `shell` | Shell command execution |
| 13 | `ApplyPatchTool` | `patch` | Codex-compatible diff DSL editing |

### Backends

| Backend | Feature | Description |
|---------|---------|-------------|
| `LocalFs` | `local-fs` | Local filesystem (tokio::fs, symlink-safe, spawn_blocking) |
| `MockFs` | (always) | In-memory for testing (zero deps, instant, deterministic) |

## Quick start

```toml
# Via sgr-agent (recommended)
sgr-agent = { version = "0.7", features = ["tools-all"] }

# Standalone
sgr-agent-tools = { version = "0.4", features = ["local-fs", "shell", "patch"] }
```

```rust
use std::sync::Arc;
use sgr_agent_tools::{LocalFs, ReadTool, WriteTool, SearchTool, TreeTool};

let fs = Arc::new(LocalFs::new("/workspace"));
let read = ReadTool(fs.clone());
let write = WriteTool(fs.clone());
let search = SearchTool(fs.clone());
```

## Testing with MockFs

```rust
use sgr_agent_tools::{MockFs, ReadTool, WriteTool};
use sgr_agent_core::agent_tool::Tool;

let fs = Arc::new(MockFs::new());
fs.add_file("readme.md", "# Hello");
fs.add_file("src/main.rs", "fn main() {}");

let read = ReadTool(fs.clone());
let result = read.execute_readonly(
    serde_json::json!({"path": "readme.md"}),
    &ctx,
).await.unwrap();
assert!(result.content.contains("Hello"));

// Assert final state
assert_eq!(fs.snapshot().len(), 2);
assert!(fs.exists("src/main.rs"));
```

## ApplyPatchTool DSL

Codex-compatible diff format — saves tokens vs full file rewrites:

```
*** Begin Patch
*** Update File: src/main.rs
@@ fn main()
-    println!("old");
+    println!("new");
*** End Patch
```

4-level fuzzy matching: exact -> trim_end -> trim -> unicode normalize.

## ShellTool

```json
{ "command": "ls -la", "workdir": "/tmp", "timeout_ms": 5000 }
```

## ReadTool indentation mode

Smart code block extraction — expand from anchor line by indent level:

```json
{ "path": "src/main.rs", "mode": "indentation", "anchor_line": 42, "max_levels": 2 }
```

## Custom tools

```rust
use sgr_agent_core::{Tool, ToolOutput, ToolError, parse_args, AgentContext, json_schema_for};
use sgr_agent_tools::FileBackend;

struct WordCountTool<B: FileBackend>(pub Arc<B>);

#[async_trait::async_trait]
impl<B: FileBackend> Tool for WordCountTool<B> {
    fn name(&self) -> &str { "word_count" }
    fn description(&self) -> &str { "Count words in a file" }
    fn is_read_only(&self) -> bool { true }
    fn parameters_schema(&self) -> serde_json::Value { json_schema_for::<Args>() }
    async fn execute(&self, args: serde_json::Value, _ctx: &mut AgentContext)
        -> Result<ToolOutput, ToolError> { /* ... */ }
}
```

## Middleware pattern

Extend base tools without forking:

```rust
struct MyReadTool<B: FileBackend> { inner: ReadTool<B> }

impl<B: FileBackend> Tool for MyReadTool<B> {
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let result = self.inner.execute(args, ctx).await?;
        Ok(ToolOutput::text(post_process(result.content)))
    }
}
```

## Features

| Feature | Default | Adds |
|---------|---------|------|
| (none) | yes | 10 core tools + MockFs |
| `eval` | no | EvalTool (Boa JS, ~5MB) |
| `shell` | no | ShellTool (tokio::process) |
| `patch` | no | ApplyPatchTool (Codex DSL) |
| `local-fs` | no | LocalFs backend (tokio::fs) |

## Architecture

```
sgr-agent-core     <- Tool, FileBackend, AgentContext (6 deps)
    ^          ^
sgr-agent-tools    sgr-agent
(this crate)       (framework, re-exports via "tools" feature)
```

## Attribution

`ApplyPatchTool` parser adapted from [Codex RS](https://github.com/openai/codex) (Apache-2.0).
`ReadTool` indentation mode inspired by Codex RS `read_file`.
