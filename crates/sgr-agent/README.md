# sgr-agent

[![Crates.io](https://img.shields.io/crates/v/sgr-agent)](https://crates.io/crates/sgr-agent)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure Rust LLM client + agent framework. Structured output, function calling, 14 file-system tools, parallel execution, 5 agent variants. Works on iOS, Android, WASM.

## Quick start

```toml
[dependencies]
# Client only
sgr-agent = "0.7"

# Agent + all tools
sgr-agent = { version = "0.7", features = ["tools-all"] }
```

### Structured output

```rust
use sgr_agent::{Llm, LlmConfig};
use sgr_agent::types::Message;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Deserialize, JsonSchema)]
struct Recipe { name: String, ingredients: Vec<String>, steps: Vec<String> }

#[tokio::main]
async fn main() {
    let llm = Llm::new(&LlmConfig::auto("gpt-4o-mini"));
    let recipe: Recipe = llm.structured(&[Message::user("Quick pasta recipe")]).await.unwrap();
    println!("{}: {} steps", recipe.name, recipe.steps.len());
}
```

### Function calling

```rust
use sgr_agent::{Llm, LlmConfig};
use sgr_agent::tool::tool;
use sgr_agent::types::Message;

#[derive(Deserialize, JsonSchema)]
struct WeatherArgs { city: String }

let llm = Llm::new(&LlmConfig::auto("gpt-4o-mini"));
let tools = vec![tool::<WeatherArgs>("get_weather", "Get weather for a city")];
let (calls, _) = llm.tools_call_stateful(&[Message::user("Weather in Tokyo?")], &tools, None).await?;
// calls[0].name == "get_weather", calls[0].arguments == {"city": "Tokyo"}
```

### Agent with tools

```rust
use std::sync::Arc;
use sgr_agent::{Llm, LlmConfig};
use sgr_agent::agents::tool_calling::ToolCallingAgent;
use sgr_agent::agent_loop::{run_loop, LoopConfig, LoopEvent};
use sgr_agent::context::AgentContext;
use sgr_agent::registry::ToolRegistry;
use sgr_agent::tools::{LocalFs, ReadTool, WriteTool, SearchTool, TreeTool, ShellTool, ApplyPatchTool};
use sgr_agent::types::Message;

let fs = Arc::new(LocalFs::new("."));
let tools = ToolRegistry::new()
    .register(ReadTool(fs.clone()))
    .register(WriteTool(fs.clone()))
    .register(SearchTool(fs.clone()))
    .register(TreeTool(fs.clone()))
    .register(ShellTool)
    .register(ApplyPatchTool(fs.clone()));

let agent = ToolCallingAgent::new(
    Llm::new(&LlmConfig::auto("gpt-4o")),
    "You are a coding assistant. Call finish() when done.",
);

let mut ctx = AgentContext::new();
let mut messages = vec![Message::user("Read README.md and summarize it")];

run_loop(&agent, &tools, &mut ctx, &mut messages, &LoopConfig::default(), |event| {
    if let LoopEvent::ToolResult { name, .. } = event { eprintln!("  {name}"); }
}).await?;
```

## Features

| Feature | Default | What |
|---------|---------|------|
| `oxide` | yes | OpenAI via [openai-oxide](https://crates.io/crates/openai-oxide) (Responses API, HTTP/2) |
| `genai` | no | Multi-provider (Gemini, Anthropic, OpenRouter, Ollama) |
| `agent` | no | Agent framework: Tool trait, ToolRegistry, agent loop, 5 variants |
| **`tools`** | no | 14 file-system tools via [`sgr-agent-tools`](https://crates.io/crates/sgr-agent-tools) |
| **`tools-eval`** | no | + JavaScript eval (Boa engine) |
| **`tools-shell`** | no | + shell command execution |
| **`tools-patch`** | no | + Codex-compatible diff editing |
| **`tools-local-fs`** | no | + `LocalFs` backend (local filesystem) |
| **`tools-all`** | no | All of the above |
| `session` | no | Session persistence, loop detection, memory, hints |
| `app-tools` | no | Built-in tools: bash, fs, git, apply_patch |
| `providers` | no | Provider config (TOML), auth |
| `telemetry` | no | OTEL tracing → Phoenix / LangSmith |

## Crate structure

```
sgr-agent-core 0.2    <- Tool, FileBackend, AgentContext (typed store), ToolError
    ^            ^        6 lightweight deps
sgr-agent-tools  sgr-agent (this crate)
0.4              LLM framework + parallel tool execution
14 tools +       re-exports core + optional tools
LocalFs + MockFs
```

| Crate | Use for |
|-------|---------|
| [`sgr-agent-core`](https://crates.io/crates/sgr-agent-core) | Building tool crates, FileBackend impls |
| [`sgr-agent-tools`](https://crates.io/crates/sgr-agent-tools) | 14 tools + LocalFs + MockFs |
| `sgr-agent` (this) | Full framework: LLM clients, agent loop, tools |

## Tools (14)

With `features = ["tools-all"]`:

| Tool | Type | Description |
|------|------|-------------|
| `ReadTool` | observe | Read file + trust metadata + indentation mode |
| `WriteTool` | act | Write file + JSON auto-repair |
| `DeleteTool` | act | Delete (single or batch) |
| `SearchTool` | observe | Smart search: fuzzy, Levenshtein, auto-expand |
| `ListTool` | observe | List directory |
| `TreeTool` | observe | Directory tree |
| `ReadAllTool` | observe | Batch read all files |
| `ShellTool` | act | Shell commands (timeout, workdir) |
| `ApplyPatchTool` | act | Codex-compatible diff DSL |
| `EvalTool` | compute | JavaScript via Boa engine |
| `MkDirTool` | act | Create directory (deferred) |
| `MoveTool` | act | Move/rename (deferred) |
| `FindTool` | observe | Find by name (deferred) |

Backends: `LocalFs` (real filesystem), `MockFs` (in-memory for tests).

## Agent variants

| Variant | Best for |
|---------|----------|
| `ToolCallingAgent` | Any FC model (simplest, recommended) |
| `SgrAgent` | Structured output via discriminated union schema |
| `HybridAgent` | 2-phase: reasoning → action (complex tasks) |
| `FlexibleAgent` | Weak/local models (text parsing, no FC needed) |
| `PlanningAgent` | Read-only exploration → structured plan |

## Creating custom tools

```rust
use sgr_agent::agent_tool::{Tool, ToolOutput, ToolError, parse_args};
use sgr_agent::context::AgentContext;
use sgr_agent::schema::json_schema_for;

#[derive(Deserialize, JsonSchema)]
struct GitStatusArgs { #[serde(default)] short: Option<bool> }

struct GitStatusTool;

#[async_trait::async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str { "git_status" }
    fn description(&self) -> &str { "Show git status" }
    fn is_read_only(&self) -> bool { true }
    fn parameters_schema(&self) -> serde_json::Value { json_schema_for::<GitStatusArgs>() }

    async fn execute(&self, args: serde_json::Value, ctx: &mut AgentContext)
        -> Result<ToolOutput, ToolError>
    {
        let a: GitStatusArgs = parse_args(&args)?;
        let output = std::process::Command::new("git").arg("status")
            .current_dir(&ctx.cwd).output().map_err(ToolError::exec)?;
        Ok(ToolOutput::text(String::from_utf8_lossy(&output.stdout)))
    }
}
```

## Testing with MockFs

```rust
use sgr_agent::tools::MockFs;

let fs = Arc::new(MockFs::new());
fs.add_file("readme.md", "# Hello");
let tool = ReadTool(fs.clone());
// tool.execute(json!({"path": "readme.md"}), &mut ctx).await
assert!(fs.exists("readme.md"));
assert_eq!(fs.snapshot().len(), 1);
```

## Examples

```bash
# Structured output (gpt-4o-mini)
cargo run -p sgr-agent --features oxide --example structured_output

# Function calling
cargo run -p sgr-agent --features oxide --example function_calling

# Full agent with tools
cargo run -p sgr-agent --features "agent,tools-all,tools-local-fs,oxide" --example agent_demo
```

## License

MIT
