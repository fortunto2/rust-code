# Project Conventions

## Architecture

Rust workspace with shared crates:
- `crates/rc-cli/` — main binary: TUI (app.rs), headless mode (main.rs), agent loop (agent.rs), 22 tools (backend.rs)
- `crates/sgr-agent/` — LLM client + agent framework + session/memory/tools/providers (all-in-one)
- `crates/sgr-agent-tui/` — shared TUI shell: chat panel, streaming, agent loop integration, fuzzy picker
- `crates/solograph/` — MCP server for code intelligence (tree-sitter, project maps)

## File Organization

```
crates/
├── rc-cli/src/agent.rs     # Agent struct, tool execution, SgrAgent impl
├── rc-cli/src/app.rs       # TUI — panels, keybindings, drawing
├── rc-cli/src/backend.rs   # LlmProvider, tool definitions, LLM call routing
├── rc-cli/src/config.rs    # Layered config (global → project → env)
├── rc-cli/src/main.rs      # CLI entry, headless mode, provider resolution
├── sgr-agent/src/          # LLM clients, agent variants, session, tools, providers
└── sgr-agent-tui/src/      # Shared TUI components
```

## Naming Conventions

- **Files**: `snake_case.rs`
- **Structs/Enums**: `PascalCase`
- **Functions/Variables**: `snake_case`
- **Constants**: `SCREAMING_SNAKE_CASE`
