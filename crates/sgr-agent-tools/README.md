# sgr-agent-tools

[![Crates.io](https://img.shields.io/crates/v/sgr-agent-tools)](https://crates.io/crates/sgr-agent-tools)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

14 reusable file-system tools for [sgr-agent](https://crates.io/crates/sgr-agent) based AI agents.

Generic over `FileBackend` trait — implement it once for your runtime (RPC, local fs, in-memory mock) and get battle-tested tools out of the box.

## Tools

### Core (always available)

| # | Tool | Type | Description |
|---|------|------|-------------|
| 1 | `ReadTool` | observe | Read file with trust metadata + **indentation-aware mode** (anchor_line, max_levels) |
| 2 | `WriteTool` | act | Write file with JSON auto-repair (llm_json) |
| 3 | `DeleteTool` | act | Delete files — single or batch via `paths[]` |
| 4 | `SearchTool` | observe | Smart search: query expansion, fuzzy regex, Levenshtein fallback, auto-expand <=10 files |
| 5 | `ListTool` | observe | List directory contents |
| 6 | `TreeTool` | observe | Directory tree structure |
| 7 | `ReadAllTool` | observe | Batch read all files in directory |
| 8 | `MkDirTool` | act | Create directory (deferred) |
| 9 | `MoveTool` | act | Move/rename file (deferred) |
| 10 | `FindTool` | observe | Find files by name pattern (deferred) |

### Optional (feature-gated)

| # | Tool | Feature | Description |
|---|------|---------|-------------|
| 11 | `EvalTool` | `eval` | JavaScript via Boa engine, file glob, workspace_date |
| 12 | `ShellTool` | `shell` | Execute shell commands (tokio::process, timeout, workdir) |
| 13 | `ApplyPatchTool` | `patch` | Codex-compatible diff DSL editing (4-level fuzzy matching) |

### ReadTool modes

| Mode | Args | Description |
|------|------|-------------|
| `slice` (default) | `start_line`, `end_line` | Line range (like `sed -n`) |
| `indentation` | `anchor_line`, `max_levels` | Smart code block extraction — expand from anchor by indent level |

## Quick start

### Via sgr-agent (recommended)

```toml
# All tools
sgr-agent = { version = "0.7", features = ["tools-all"] }

# Pick what you need
sgr-agent = { version = "0.7", features = ["tools"] }           # core 10 tools
sgr-agent = { version = "0.7", features = ["tools-eval"] }      # + JS eval
sgr-agent = { version = "0.7", features = ["tools-shell"] }     # + shell exec
sgr-agent = { version = "0.7", features = ["tools-patch"] }     # + apply_patch
```

```rust
use sgr_agent::tools::{FileBackend, ReadTool, SearchTool, WriteTool, ShellTool, ApplyPatchTool};
```

### Standalone

```toml
sgr-agent-tools = { version = "0.2", features = ["eval", "shell", "patch"] }
```

## Usage

```rust
use std::sync::Arc;
use sgr_agent_tools::{FileBackend, ReadTool, WriteTool, SearchTool, TreeTool};

struct MyBackend; // implement FileBackend for your runtime

let backend = Arc::new(MyBackend);
let read = ReadTool(backend.clone());
let write = WriteTool(backend.clone());
let search = SearchTool(backend.clone());
```

## ApplyPatchTool DSL

Codex-compatible diff format. Saves tokens vs full file rewrites.

```
*** Begin Patch
*** Add File: src/new.rs
+fn hello() {}
+
*** Delete File: src/old.rs
*** Update File: src/main.rs
@@ fn main()
-    println!("old");
+    println!("new");
*** End Patch
```

Operations: `Add File`, `Delete File`, `Update File` (with optional `Move to`).
Context matching: exact -> trim_end -> trim -> unicode normalize (4 levels).

## ShellTool

```json
{ "command": "ls -la", "workdir": "/tmp", "timeout_ms": 5000 }
```

Returns exit code + combined stdout/stderr. Timeout default: 120s, max: 600s.

## Adding custom tools

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

Extend base tools with project-specific behavior (guards, hooks, annotations) without forking:

```rust
struct MyReadTool<B: FileBackend> {
    inner: ReadTool<B>,
    // add your state
}

impl<B: FileBackend> Tool for MyReadTool<B> {
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let result = self.inner.execute(args, ctx).await?;
        // post-process: add security scan, workflow tracking, etc.
        Ok(result)
    }
}
```

## Design principles

- **7 core tools max** in prompt schema — models degrade on long tool lists
- **Deferred loading** for rarely-used tools (mkdir, move, find)
- **Trust metadata** on every read: `[path | trusted/untrusted]`
- **Batch tools** save round-trips (read_all: 48->4 calls)
- **Smart search** — don't fail silently, try name variants and fuzzy matching
- **JSON auto-repair** — LLMs produce broken JSON, fix before writing
- **Diff editing** — apply_patch saves tokens vs full file writes

## Features

| Feature | Default | What |
|---------|---------|------|
| (none) | yes | 10 core tools |
| `eval` | no | EvalTool — Boa JS engine (~5MB) |
| `shell` | no | ShellTool — tokio::process |
| `patch` | no | ApplyPatchTool — Codex-compatible diff DSL |

## Architecture

```
sgr-agent-core    <- Tool trait, AgentContext, schema (5 deps)
    ^         ^
sgr-agent-tools   sgr-agent
(this crate)      (framework, re-exports via "tools" feature)
```

## Attribution

`ApplyPatchTool` parser adapted from [Codex RS](https://github.com/openai/codex) (Apache-2.0 license).
`ReadTool` indentation mode algorithm inspired by Codex RS `read_file` handler.
