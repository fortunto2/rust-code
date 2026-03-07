# Project Conventions

## Architecture

Rust workspace with shared crate:
- `crates/rc-cli/` — main binary: TUI (app.rs), headless mode (main.rs), agent loop (agent.rs)
- `crates/rc-baml/` — BAML schemas (.baml) and generated client
- `crates/baml-agent/` — shared SGR agent library (session, loop detection, helpers)

## File Organization

```
crates/
├── rc-baml/baml_src/     # All BAML schemas (tools, prompt, clients)
├── rc-cli/src/agent.rs   # Agent struct, tool execution, SgrAgent impl
├── rc-cli/src/app.rs     # TUI — panels, keybindings, drawing
├── rc-cli/src/main.rs    # CLI entry, headless mode, sessions command
└── baml-agent/src/       # Shared: session, loop detect, prompt, helpers
```

## Naming Conventions

- **Files**: `snake_case.rs`
- **Structs/Enums**: `PascalCase`
- **Functions/Variables**: `snake_case`
- **Constants**: `SCREAMING_SNAKE_CASE`
