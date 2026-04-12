# sgr-agent-tools

[![Crates.io](https://img.shields.io/crates/v/sgr-agent-tools)](https://crates.io/crates/sgr-agent-tools)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

11 reusable file-system tools for [sgr-agent](https://crates.io/crates/sgr-agent) based AI agents.

Generic over `FileBackend` trait — implement it once for your runtime (RPC, local fs, in-memory mock) and get battle-tested tools out of the box.

## Tools

| # | Tool | Type | Description |
|---|------|------|-------------|
| 1 | `ReadTool` | observe | Read file with trust metadata header |
| 2 | `WriteTool` | act | Write file with JSON auto-repair (llm_json) |
| 3 | `DeleteTool` | act | Delete files — single or batch via `paths[]` |
| 4 | `SearchTool` | observe | Smart search: query expansion, fuzzy regex, Levenshtein fallback, auto-expand ≤10 files |
| 5 | `ListTool` | observe | List directory contents |
| 6 | `TreeTool` | observe | Directory tree structure |
| 7 | `EvalTool` | compute | JavaScript via Boa engine, file glob, workspace_date (feature `eval`) |
| 8 | `ReadAllTool` | observe | Batch read all files in directory |
| 9 | `MkDirTool` | act | Create directory (deferred) |
| 10 | `MoveTool` | act | Move/rename file (deferred) |
| 11 | `FindTool` | observe | Find files by name pattern (deferred) |

## Quick start

### Via sgr-agent (recommended)

```toml
sgr-agent = { version = "0.7", features = ["tools"] }
# with JS eval:
sgr-agent = { version = "0.7", features = ["tools-eval"] }
```

```rust
use sgr_agent::tools::{FileBackend, ReadTool, SearchTool, WriteTool};
```

### Standalone

```toml
sgr-agent-tools = "0.1"
# with JS eval:
sgr-agent-tools = { version = "0.1", features = ["eval"] }
```

## Usage

```rust
use std::sync::Arc;
use sgr_agent_tools::{FileBackend, ReadTool, WriteTool, SearchTool, TreeTool};

// 1. Implement FileBackend for your runtime
struct MyBackend;

#[async_trait::async_trait]
impl FileBackend for MyBackend {
    async fn read(&self, path: &str, number: bool, start_line: i32, end_line: i32) -> anyhow::Result<String> {
        todo!("read from your storage")
    }
    async fn write(&self, path: &str, content: &str, start_line: i32, end_line: i32) -> anyhow::Result<()> {
        todo!()
    }
    async fn delete(&self, path: &str) -> anyhow::Result<()> { todo!() }
    async fn search(&self, root: &str, pattern: &str, limit: i32) -> anyhow::Result<String> { todo!() }
    async fn list(&self, path: &str) -> anyhow::Result<String> { todo!() }
    async fn tree(&self, root: &str, level: i32) -> anyhow::Result<String> { todo!() }
    async fn context(&self) -> anyhow::Result<String> { todo!() }
    async fn mkdir(&self, path: &str) -> anyhow::Result<()> { todo!() }
    async fn move_file(&self, from: &str, to: &str) -> anyhow::Result<()> { todo!() }
    async fn find(&self, root: &str, name: &str, file_type: &str, limit: i32) -> anyhow::Result<String> { todo!() }
}

// 2. Create tools
let backend = Arc::new(MyBackend);
let read = ReadTool(backend.clone());
let write = WriteTool(backend.clone());
let search = SearchTool(backend.clone());
let tree = TreeTool(backend.clone());
```

## Adding your own tools

Build custom tools using `sgr-agent-core` types:

```rust
use std::sync::Arc;
use sgr_agent_core::{Tool, ToolOutput, ToolError, parse_args, AgentContext, json_schema_for};
use schemars::JsonSchema;
use serde::Deserialize;

use sgr_agent_tools::FileBackend;

#[derive(Deserialize, JsonSchema)]
struct WordCountArgs {
    /// File path to count words in
    path: String,
}

struct WordCountTool<B: FileBackend>(pub Arc<B>);

#[async_trait::async_trait]
impl<B: FileBackend> Tool for WordCountTool<B> {
    fn name(&self) -> &str { "word_count" }
    fn description(&self) -> &str { "Count words in a file" }
    fn is_read_only(&self) -> bool { true }
    fn parameters_schema(&self) -> serde_json::Value { json_schema_for::<WordCountArgs>() }

    async fn execute(&self, args: serde_json::Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: WordCountArgs = parse_args(&args)?;
        let content = self.0.read(&a.path, false, 0, 0).await
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        let count = content.split_whitespace().count();
        Ok(ToolOutput::text(format!("{count} words")))
    }
}
```

Pattern: `struct YourTool<B: FileBackend>(pub Arc<B>)` — generic over backend, reusable across projects.

## Design principles

Based on building a PAC1 benchmark agent (16→11 tools, 7 models, 40+ tasks) and studying Codex CLI / Claude Code:

- **7 core tools max** in prompt schema — models degrade on long tool lists
- **Deferred loading** for rarely-used tools (mkdir, move, find)
- **Trust metadata** on every read: `[path | trusted/untrusted]`
- **Batch tools** justified only when saving 3+ round-trips (read_all: 48→4 calls)
- **Smart search** — don't fail silently, try name variants and fuzzy matching
- **JSON auto-repair** — LLMs produce broken JSON, fix it before writing

## Crate architecture

```
sgr-agent-core    ← Tool trait, AgentContext, schema (5 lightweight deps)
    ↑         ↑
sgr-agent-tools   sgr-agent
(this crate)      (framework, re-exports tools via feature "tools")
```

## Features

| Feature | Default | What |
|---------|---------|------|
| (none) | yes | 10 tools without JS eval |
| `eval` | no | Adds `EvalTool` — Boa JS engine (~5MB binary size) |
