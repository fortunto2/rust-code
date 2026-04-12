# sgr-agent

[![Crates.io](https://img.shields.io/crates/v/sgr-agent)](https://crates.io/crates/sgr-agent)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure Rust LLM client and agent framework based on [Schema-Guided Reasoning (SGR)](https://abdullin.com/schema-guided-reasoning/) by [Rinat Abdullin](https://abdullin.com). No dlopen, no external binaries.
Works on iOS, Android, WASM — anywhere `reqwest` + `rustls` compiles.

## Three backends

| Backend | Feature | API | Best for |
|---------|---------|-----|----------|
| **[openai-oxide](https://github.com/fortunto2/openai-rust)** | `oxide` | Responses API | OpenAI models (fastest — HTTP/2 keep-alive, WebSocket, hedged requests) |
| **genai** | `genai` | Chat Completions / Responses | Multi-provider (Gemini, Anthropic, OpenRouter, Ollama) |
| **async-openai** | `async-openai-backend` | Responses API | Migration from async-openai |

`Llm::new()` **auto-selects** backend by model name:
- `gpt-*`, `o3*`, `o4*`, `chatgpt-*` → **oxide** (Responses API, gzip, HTTP/2 keep-alive)
- Everything else → **genai** (multi-provider)
- Custom `base_url` / Vertex AI → always **genai**

```rust
let llm = Llm::new(&LlmConfig::auto("gpt-5.4"));        // → oxide
let llm = Llm::new(&LlmConfig::auto("gemini-2.0-flash")); // → genai
println!("{}", llm.backend_name()); // "oxide" or "genai"
```

With `oxide-ws` feature, upgrade to WebSocket for -20% latency in agent loops:
```rust
let oxide = OxideClient::from_config(&config)?;
oxide.connect_ws().await?;  // all calls now go through wss://
```

See [openai-oxide benchmarks](https://github.com/fortunto2/openai-rust#performance) — wins 10/13 vs Python.

## Two layers

**Layer 1 — LLM Client** (default features: `gemini`, `openai`):
structured output, function calling, flexible parsing. Just add a dependency and call an API.

**Layer 2 — Agent Framework** (feature: `agent`):
Tool trait, registry, agent loop with loop detection, 4 agent variants, dual-model routing, retry, streaming.
Build autonomous agents that reason and act.

## Quick start

```toml
# Cargo.toml

# Client only (structured output + function calling)
sgr-agent = "0.4"

# Full agent framework
sgr-agent = { version = "0.2", features = ["agent"] }
```

### Structured output (client only)

```rust
use sgr_agent::gemini::GeminiClient;
use sgr_agent::ProviderConfig;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(JsonSchema, Deserialize)]
struct Recipe {
    name: String,
    ingredients: Vec<String>,
    steps: Vec<String>,
}

#[tokio::main]
async fn main() {
    let client = GeminiClient::new(
        ProviderConfig::gemini("YOUR_API_KEY", "gemini-3.1-pro-preview")
    );

    let response = client
        .structured::<Recipe>(&[("user", "Give me a pasta recipe")], None)
        .await
        .unwrap();

    println!("{}: {} steps", response.output.name, response.output.steps.len());
}
```

### Agent with tools

```rust
use sgr_agent::agent_loop::{run_loop, LoopConfig, LoopEvent};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput};
use sgr_agent::agents::sgr::SgrAgent;
use sgr_agent::context::AgentContext;
use sgr_agent::gemini::GeminiClient;
use sgr_agent::registry::ToolRegistry;
use sgr_agent::types::Message;
use sgr_agent::ProviderConfig;
use serde_json::Value;

struct ReadFile;

#[async_trait::async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str { "read_file" }
    fn description(&self) -> &str { "Read a file from disk" }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to read" }
            },
            "required": ["path"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let path = args["path"].as_str().ok_or(ToolError::InvalidArgs("missing path".into()))?;
        match std::fs::read_to_string(path) {
            Ok(content) => Ok(ToolOutput::text(content)),
            Err(e) => Ok(ToolOutput::text(format!("Error: {e}"))),
        }
    }
}

struct Finish;

#[async_trait::async_trait]
impl Tool for Finish {
    fn name(&self) -> &str { "finish_task" }
    fn description(&self) -> &str { "Call when the task is complete" }
    fn is_system(&self) -> bool { true }
    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string" }
            },
            "required": ["summary"]
        })
    }
    async fn execute(&self, args: Value, _: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::done(args["summary"].as_str().unwrap_or("Done")))
    }
}

#[tokio::main]
async fn main() {
    let client = GeminiClient::new(
        ProviderConfig::gemini("YOUR_API_KEY", "gemini-3.1-pro-preview")
    );

    let tools = ToolRegistry::new()
        .register(ReadFile)
        .register(Finish);

    let agent = SgrAgent::new(client, "You are a coding assistant.");

    let mut ctx = AgentContext::new();
    let mut messages = vec![Message::user("Read main.rs and summarize it")];
    let config = LoopConfig { max_steps: 10, ..Default::default() };

    run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |event| {
        match event {
            LoopEvent::StepStart { step } => eprintln!("step {step}"),
            LoopEvent::ToolResult { name, output } => {
                eprintln!("  {name} -> {}...", &output[..output.len().min(100)]);
            }
            LoopEvent::Completed { steps } => eprintln!("done in {steps} steps"),
            _ => {}
        }
    }).await.unwrap();
}
```

## Features

| Feature | Default | What |
|---------|---------|------|
| `gemini` | yes | Google AI + Vertex AI backend |
| `openai` | yes | OpenAI + OpenRouter + Ollama backend |
| `oxide` | no | **openai-oxide backend — fastest for OpenAI models** (Responses API, HTTP/2, gzip) |
| `oxide-ws` | no | WebSocket mode for oxide (-20% latency on agent loops) |
| `genai` | no | Multi-provider via genai crate (Gemini, Anthropic, OpenRouter, Ollama) |
| `agent` | no | Full agent framework (traits, loop, registry, routing) |
| **`tools`** | no | **14 file-system tools** via [`sgr-agent-tools`](https://crates.io/crates/sgr-agent-tools) — generic over `FileBackend` |
| **`tools-eval`** | no | `tools` + JavaScript eval via Boa engine |
| **`tools-shell`** | no | `tools` + shell command execution (tokio::process) |
| **`tools-patch`** | no | `tools` + Codex-compatible diff editing (apply_patch DSL) |
| **`tools-all`** | no | All tools: eval + shell + patch |
| `session` | no | Session persistence, 4-tier loop detection, memory context, hints, tasks, intent guard |
| `app-tools` | no | Shared tools: bash, fs (read/write/edit), git, apply_patch |
| `providers` | no | Provider config (TOML), auth, CLI proxy, Codex proxy |
| `telemetry` | no | OTEL telemetry → Phoenix / LangSmith (OpenInference conventions) |
| `logging` | no | File-based JSONL logging |
| `search` | no | Fuzzy session search (nucleo-matcher) |

## Crate structure

```
sgr-agent-core 0.2   <- Tool, FileBackend, AgentContext (typed store), ToolError (6 deps)
    ^            ^
sgr-agent-tools  sgr-agent (this crate)
0.4              LLM framework + parallel tool execution
14 tools +       re-exports core + optional tools
LocalFs + MockFs
```

| Crate | When to use |
|-------|-------------|
| [`sgr-agent-core`](https://crates.io/crates/sgr-agent-core) | Building a tool crate or FileBackend impl |
| [`sgr-agent-tools`](https://crates.io/crates/sgr-agent-tools) | 14 tools + LocalFs + MockFs for testing |
| `sgr-agent` (this) | Full framework: LLM clients, agent loop, registry, tools |

## Using file-system tools

```toml
sgr-agent = { version = "0.7", features = ["tools"] }
```

```rust
use std::sync::Arc;
use sgr_agent::tools::{FileBackend, ReadTool, WriteTool, SearchTool, TreeTool, ListTool, DeleteTool, ReadAllTool};
use sgr_agent::registry::ToolRegistry;

// 1. Implement FileBackend for your runtime
struct LocalFs;

#[async_trait::async_trait]
impl FileBackend for LocalFs {
    async fn read(&self, path: &str, number: bool, start: i32, end: i32) -> anyhow::Result<String> {
        Ok(std::fs::read_to_string(path)?)
    }
    async fn write(&self, path: &str, content: &str, _s: i32, _e: i32) -> anyhow::Result<()> {
        Ok(std::fs::write(path, content)?)
    }
    async fn delete(&self, path: &str) -> anyhow::Result<()> {
        Ok(std::fs::remove_file(path)?)
    }
    async fn search(&self, _root: &str, _pattern: &str, _limit: i32) -> anyhow::Result<String> {
        Ok(String::new()) // use ripgrep or walkdir
    }
    async fn list(&self, path: &str) -> anyhow::Result<String> {
        let entries: Vec<_> = std::fs::read_dir(path)?
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        Ok(entries.join("\n"))
    }
    async fn tree(&self, _root: &str, _level: i32) -> anyhow::Result<String> { Ok(String::new()) }
    async fn context(&self) -> anyhow::Result<String> { Ok(chrono::Local::now().to_rfc3339()) }
    async fn mkdir(&self, path: &str) -> anyhow::Result<()> { Ok(std::fs::create_dir_all(path)?) }
    async fn move_file(&self, from: &str, to: &str) -> anyhow::Result<()> { Ok(std::fs::rename(from, to)?) }
    async fn find(&self, _root: &str, _name: &str, _ft: &str, _limit: i32) -> anyhow::Result<String> { Ok(String::new()) }
}

// 2. Create tools and register
let fs = Arc::new(LocalFs);
let tools = ToolRegistry::new()
    .register(ReadTool(fs.clone()))
    .register(WriteTool(fs.clone()))
    .register(DeleteTool(fs.clone()))
    .register(SearchTool(fs.clone()))
    .register(ListTool(fs.clone()))
    .register(TreeTool(fs.clone()))
    .register(ReadAllTool(fs.clone()));
```

## Creating custom tools

Tools implement the `Tool` trait from `sgr-agent-core`:

```rust
use sgr_agent::agent_tool::{Tool, ToolOutput, ToolError, parse_args};
use sgr_agent::context::AgentContext;
use sgr_agent::schema::json_schema_for;
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct GitStatusArgs {
    /// Repository path (default: current directory)
    #[serde(default)]
    path: Option<String>,
}

struct GitStatusTool;

#[async_trait::async_trait]
impl Tool for GitStatusTool {
    fn name(&self) -> &str { "git_status" }
    fn description(&self) -> &str { "Show git repository status" }
    fn is_read_only(&self) -> bool { true }
    fn parameters_schema(&self) -> serde_json::Value { json_schema_for::<GitStatusArgs>() }

    async fn execute(&self, args: serde_json::Value, _ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        let a: GitStatusArgs = parse_args(&args)?;
        let path = a.path.unwrap_or_else(|| ".".into());
        let output = tokio::process::Command::new("git")
            .args(["status", "--short"])
            .current_dir(&path)
            .output()
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        Ok(ToolOutput::text(String::from_utf8_lossy(&output.stdout)))
    }
}
```

Key patterns:
- Use `#[derive(Deserialize, JsonSchema)]` for args — schema auto-generated
- Use `parse_args(&args)` for typed deserialization with error handling
- Use `json_schema_for::<T>()` for parameter schema
- `is_read_only() -> true` enables parallel execution
- `ToolOutput::text()` for normal results, `ToolOutput::done()` to signal completion

## Architecture

### LLM Client layer

| Module | What |
|--------|------|
| `gemini` | Gemini client — Google AI (`generativelanguage.googleapis.com`) and Vertex AI (`aiplatform.googleapis.com`) |
| `openai` | OpenAI-compatible client — works with OpenAI, OpenRouter, Ollama, any compatible API |
| `types` | `Message`, `ToolCall`, `SgrError`, `ProviderConfig`, `RateLimitInfo` |
| `tool` | `ToolDef` — tool definition (name, description, JSON Schema parameters) |
| `schema` | `json_schema_for::<T>()` — derive JSON Schema from Rust types via `schemars` |
| `flexible_parser` | Extract JSON from markdown blocks, broken JSON, streaming chunks, chain-of-thought text |
| `coerce` | Fuzzy type coercion — `"42"` → `42`, `"true"` → `true`, fuzzy enum matching |

### Agent Framework layer (`feature = "agent"`)

| Module | What |
|--------|------|
| `agent` | `Agent` trait with `decide()` + lifecycle hooks (`prepare_context`, `prepare_tools`, `after_action`) |
| `agent_tool` | `Tool` trait — `name()`, `description()`, `parameters_schema()`, `execute()` |
| `agent_loop` | `run_loop()` — decide → execute → feed back, with 3-tier loop detection + auto-completion + sliding window |
| `registry` | `ToolRegistry` — ordered collection, case-insensitive lookup, fuzzy resolve, filtering |
| `context` | `AgentContext` — working directory, state machine, per-tool config, custom metadata |
| `client` | `LlmClient` trait — abstraction over any LLM backend |
| `agents/sgr` | `SgrAgent` — structured output via discriminated union schema |
| `agents/tool_calling` | `ToolCallingAgent` — native function calling (simplest variant) |
| `agents/flexible` | `FlexibleAgent` — text parsing with retry and error feedback (for weak models) |
| `agents/hybrid` | `HybridAgent` — 2-phase: reasoning-only FC → full toolkit with reasoning context |
| `agents/planning` | `PlanningAgent` — read-only wrapper that produces structured plans (like Claude Code plan mode) |
| `agents/clarification` | `ClarificationTool` + `PlanTool` — built-in system tools for interactive agents |
| `router` | `ModelRouter` — transparent dual-model routing (smart for complex, fast for simple tasks) |
| `retry` | `RetryClient` — exponential backoff with jitter, honors `Retry-After` headers |
| `factory` | `AgentFactory` — create agents from JSON config |
| `discovery` | `ToolFilter` — progressive tool discovery via keyword/TF-IDF scoring |
| `streaming` | `StreamingSender`/`StreamingReceiver` — channel-based event streaming |
| `schema_simplifier` | Convert JSON Schema to human-readable text (for FlexibleAgent prompts) |
| `union_schema` | Build discriminated union JSON Schema from tool definitions at runtime |

## Agent variants

### SgrAgent (structured output)

Best for capable models (Gemini 3.1 Pro, GPT-4o). Builds a discriminated union JSON Schema from your tools at runtime, sends via `structured_call`, parses response with flexible parser + coercion.

```rust
let agent = SgrAgent::new(client, "You are a helpful assistant.");
```

### ToolCallingAgent (native function calling)

Simplest variant. Sends tools via native FC API, gets `Vec<ToolCall>` back directly. Works with any model that supports function calling.

```rust
let agent = ToolCallingAgent::new(client, "You are a helpful assistant.");
```

### FlexibleAgent (text parsing)

For weak models or text-only backends (Ollama, local models). Puts tool descriptions in the system prompt as human-readable text, parses JSON from model's free-form response. Includes retry with error feedback.

```rust
let agent = FlexibleAgent::new(client, "You are a helpful assistant.");
```

### HybridAgent (2-phase reasoning)

Two-phase approach: Phase 1 calls a "reasoning" tool only (think step), Phase 2 sends the full toolkit with reasoning context. Best for complex multi-step tasks.

```rust
let agent = HybridAgent::new(client, "You are a helpful assistant.");
```

### PlanningAgent (read-only plan mode)

Wraps any agent to restrict tools to a read-only subset. The agent explores the codebase, then calls `submit_plan` with a structured plan. Like Claude Code's plan mode.

```rust
use sgr_agent::agents::planning::{PlanningAgent, Plan};
use sgr_agent::agents::clarification::{ClarificationTool, PlanTool};

let inner = SgrAgent::new(client, "You are an architect. Analyze and create an implementation plan.");
let planner = PlanningAgent::new(Box::new(inner));

let tools = ToolRegistry::new()
    .register(ReadFile)
    .register(ListDir)
    .register(SearchCode)
    .register(PlanTool)           // submit_plan — produces structured Plan
    .register(ClarificationTool); // ask_user — pause for questions

run_loop(&planner, &tools, &mut ctx, &mut messages, &config, |_| {}).await?;

// Extract the plan after completion
let plan = Plan::from_context(&ctx).unwrap();
println!("{}", plan.summary);
for (i, step) in plan.steps.iter().enumerate() {
    println!("{}. {} (files: {:?})", i + 1, step.description, step.files);
}

// Inject plan into build agent's context
let plan_msg = plan.to_message();
build_messages.insert(1, plan_msg);
```

## Interactive agents (clarification)

Use `run_loop_interactive` when the agent may need to ask the user questions:

```rust
use sgr_agent::agent_loop::run_loop_interactive;
use sgr_agent::agents::clarification::ClarificationTool;

let tools = ToolRegistry::new()
    .register(ReadFile)
    .register(WriteFile)
    .register(ClarificationTool) // ask_user tool
    .register(Finish);

// Async callback — called when agent needs user input
run_loop_interactive(
    &agent, &tools, &mut ctx, &mut messages, &config,
    |event| { /* handle events */ },
    |question| async move {
        println!("Agent asks: {}", question);
        // Get user input (from stdin, GUI, API, etc.)
        let mut input = String::new();
        std::io::stdin().read_line(&mut input).unwrap();
        input.trim().to_string()
    },
).await?;
```

The regular `run_loop` also supports `WaitingForInput` events but continues with a placeholder instead of pausing.

## Dual-model routing

Use a smart model for complex decisions and a fast model for simple ones:

```rust
use sgr_agent::router::{ModelRouter, RouterConfig};

let router = ModelRouter::new(
    GeminiClient::new(ProviderConfig::gemini(&key, "gemini-3.1-pro-preview")),
    GeminiClient::new(ProviderConfig::gemini(&key, "gemini-3.1-flash-lite-preview")),
).with_config(RouterConfig {
    message_threshold: 10,  // use smart when < 10 messages
    tool_threshold: 8,      // use smart when < 8 tools
    always_smart: false,
});

// Use router as any LlmClient — routing is transparent
let agent = SgrAgent::new(router, "You are a helpful assistant.");
```

## Retry with backoff

Wrap any client with automatic retry on transient errors (rate limits, 5xx, timeouts):

```rust
use sgr_agent::retry::{RetryClient, RetryConfig};

let client = RetryClient::new(GeminiClient::new(config))
    .with_config(RetryConfig {
        max_retries: 3,
        base_delay_ms: 500,
        max_delay_ms: 30_000,
    });
```

Honors `Retry-After` headers from rate limit responses.

## Agent loop

The loop drives the agent: decide → execute tools → feed results back → repeat.

```rust
use sgr_agent::agent_loop::{run_loop, LoopConfig};

let config = LoopConfig {
    max_steps: 50,              // hard limit on iterations
    loop_abort_threshold: 6,    // abort after 6 consecutive identical actions
    max_messages: 80,           // sliding window — trim old messages
    auto_complete_threshold: 3, // auto-complete if situation repeats 3x
};

let steps = run_loop(&agent, &tools, &mut ctx, &mut messages, &config, |event| {
    // handle events: StepStart, Decision, ToolResult, Completed, LoopDetected, Error
}).await?;
```

**3-tier loop detection:**
1. **Exact signature** — same tool call sequence repeats N times
2. **Tool frequency** — single tool dominates >90% of all calls
3. **Output stagnation** — tool outputs are identical across steps

**Auto-completion detection:**
- Catches agents that finished but forgot to call `finish_task`
- Keyword detection ("task is complete", "all done", etc.)
- Repeated situation text (agent stuck describing same state)

**Sliding window:**
- Keeps first 2 messages (system + user prompt) + last N
- Inserts a summary marker where messages were trimmed

## Agent lifecycle hooks

Override hooks on the `Agent` trait for cross-cutting concerns:

```rust
impl Agent for MyAgent {
    async fn decide(&self, messages: &[Message], tools: &ToolRegistry) -> Result<Decision, AgentError> {
        // your decision logic
    }

    fn prepare_context(&self, ctx: &mut AgentContext, messages: &[Message]) {
        // inject context before each decision (e.g., update working state)
    }

    fn prepare_tools(&self, ctx: &AgentContext, tools: &ToolRegistry) -> Vec<String> {
        // filter/reorder tools based on context (return tool names to include)
        tools.list().iter().map(|t| t.name().to_string()).collect()
    }

    fn after_action(&self, ctx: &mut AgentContext, tool_name: &str, output: &str) {
        // post-action hook (e.g., log, update state, track changes)
    }
}
```

## Vertex AI

```rust
let config = ProviderConfig::vertex(
    "ACCESS_TOKEN",           // from `gcloud auth print-access-token`
    "my-gcp-project",
    "gemini-3.1-pro-preview",
);
// Default region: "global" (aiplatform.googleapis.com)
```

## Flexible parser

The flexible parser extracts JSON from messy LLM output — markdown blocks, broken JSON, streaming chunks, chain-of-thought wrapping:

```rust
use sgr_agent::{parse_flexible, parse_flexible_coerced};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(JsonSchema, Deserialize)]
struct Output { answer: String }

// Handles: ```json {...} ```, bare JSON, broken brackets, single quotes, trailing commas
let result: Output = parse_flexible_coerced(
    r#"Here's my answer: ```json {"answer": "42"} ```"#,
    &schema,
)?;
```

## Progressive discovery

Filter tools by relevance when you have many tools but want to show only the most relevant ones:

```rust
use sgr_agent::discovery::ToolFilter;

let filter = ToolFilter::new(5); // show max 5 tools
let relevant = filter.select("read the config file", &registry);
// Returns: system tools (always) + top-scored tools by keyword overlap
```

## Running the example

A full 15-tool coding agent demo is included:

```bash
# With Google AI
export GEMINI_API_KEY=your_key
cargo run -p sgr-agent --features agent --example agent_demo -- "Create a hello world Python script"

# With Vertex AI
export GOOGLE_CLOUD_PROJECT=my-project
cargo run -p sgr-agent --features agent --example agent_demo -- "Create a hello world Python script"
```

The example includes: ReadFile, WriteFile, EditFile, ListDir, Bash (with 30s timeout), BackgroundTask, SearchCode, Grep, Glob, GitDiff, GitStatus, GitLog, GetCwd, ChangeDir, FinishTask.

## Standalone project example

```toml
# Cargo.toml
[package]
name = "my-agent"
version = "0.1.0"
edition = "2021"

[dependencies]
sgr-agent = { version = "0.2", features = ["agent", "gemini"] }
serde_json = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
async-trait = "0.1"
```

See [`/tmp/my-agent`](https://github.com/fortunto2/rust-code/tree/master/crates/sgr-agent/examples) for a full working standalone project.

## License

MIT
