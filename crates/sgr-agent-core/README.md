# sgr-agent-core

[![Crates.io](https://img.shields.io/crates/v/sgr-agent-core)](https://crates.io/crates/sgr-agent-core)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Core types for the [sgr-agent](https://crates.io/crates/sgr-agent) ecosystem — the minimal shared interface that both the framework and tool crates depend on.

## What's inside

| Module | Types | Purpose |
|--------|-------|---------|
| `agent_tool` | `Tool`, `ToolOutput`, `ToolError`, `ContextModifier`, `parse_args` | Tool trait and execution types |
| `context` | `AgentContext`, `AgentState` | Shared state passed to tools |
| `schema` | `json_schema_for`, `response_schema_for`, `make_openai_strict` | JSON Schema from Rust types |
| `tool` | `ToolDef`, `tool()` | Tool definitions for LLM APIs |

## When to use

- **Building an agent app?** Use [`sgr-agent`](https://crates.io/crates/sgr-agent) — it re-exports everything from core.
- **Building a tools crate?** Depend on `sgr-agent-core` to avoid circular deps with sgr-agent.

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
        // ... ping logic
        Ok(ToolOutput::text(format!("{}: 12ms", a.host)))
    }
}
```

## Crate graph

```
sgr-agent-core (this crate)  ← 5 lightweight deps
    ↑              ↑
sgr-agent-tools    sgr-agent
(file tools)       (framework)
```

## Dependencies

Only 5 lightweight crates — no reqwest, no tokio, no LLM clients:

- `async-trait` — async trait methods
- `serde` + `serde_json` — JSON serialization
- `schemars` — JSON Schema generation
- `thiserror` — error derives
