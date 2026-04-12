# sgr-agent-core

[![Crates.io](https://img.shields.io/crates/v/sgr-agent-core)](https://crates.io/crates/sgr-agent-core)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Core types for the [sgr-agent](https://crates.io/crates/sgr-agent) ecosystem — the minimal shared interface that framework, tools, and custom tool crates all depend on.

## What's inside

| Module | Types | Purpose |
|--------|-------|---------|
| `agent_tool` | `Tool`, `ToolOutput`, `ToolError`, `ContextModifier`, `parse_args` | Tool trait and execution types |
| `backend` | `FileBackend` | Filesystem abstraction (implement for your runtime) |
| `context` | `AgentContext`, `AgentState` | Shared state — typed store + observations + tool cache |
| `schema` | `json_schema_for`, `response_schema_for`, `make_openai_strict` | JSON Schema from Rust types |
| `tool` | `ToolDef`, `tool()` | Tool definitions for LLM APIs |

## When to use

- **Building an agent app?** Use [`sgr-agent`](https://crates.io/crates/sgr-agent) — re-exports everything.
- **Building a tools crate?** Depend on `sgr-agent-core` directly (avoids circular deps).
- **Implementing a FileBackend?** Depend on `sgr-agent-core` — the trait lives here.

## Creating a tool

```rust
use sgr_agent_core::{Tool, ToolOutput, ToolError, parse_args, AgentContext, json_schema_for};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
struct PingArgs {
    /// Host to ping
    host: String,
}

struct PingTool;

#[async_trait::async_trait]
impl Tool for PingTool {
    fn name(&self) -> &str { "ping" }
    fn description(&self) -> &str { "Ping a host and return latency" }
    fn is_read_only(&self) -> bool { true }
    fn parameters_schema(&self) -> serde_json::Value { json_schema_for::<PingArgs>() }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: PingArgs = parse_args(&args)?;
        Ok(ToolOutput::text(format!("{}: 12ms", a.host)))
    }
}
```

## Typed context store

No string-key collisions — each type gets its own slot:

```rust
#[derive(Clone)]
struct MyToolState { count: usize }

ctx.insert(MyToolState { count: 0 });
let state = ctx.get_typed::<MyToolState>().unwrap();
```

Legacy `ctx.set("key", json_value)` / `ctx.get("key")` still works for compat.

## ToolError variants

| Variant | When |
|---------|------|
| `Execution(String)` | I/O, network, logic error |
| `InvalidArgs(String)` | Args parse/validation failed |
| `PermissionDenied(String)` | Sandbox, policy, auth |
| `NotFound(String)` | File/resource not found |
| `Timeout(String)` | Operation timed out |

## Crate graph

```
sgr-agent-core (this crate)  <- 6 lightweight deps
    ^              ^
sgr-agent-tools    sgr-agent
(14 tools +        (LLM framework, re-exports core + tools)
 LocalFs + MockFs)
```

## Dependencies

6 lightweight crates — no reqwest, no tokio, no LLM clients:

`async-trait`, `serde`, `serde_json`, `schemars`, `thiserror`, `anyhow`
