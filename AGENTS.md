# Agent Instructions (rust-code)

## Stack
- Rust (Edition 2024)
- Tokio (Async runtime)
- Ratatui + Crossterm (Terminal UI)
- BAML (Schema-Guided Reasoning)
- Nucleo (Fuzzy search)

## Rules
- **BAML First**: Any changes to LLM prompts or Tool schemas MUST be done in `crates/rc-baml/baml_src/`.
- **BAML Generation**: After editing `.baml` files, you MUST run `npx @boundaryml/baml@0.218.0 generate` in `crates/rc-baml`.
- **TUI Architecture**: UI runs on the main thread (`rc-cli`), Agent runs on a background Tokio task (`rc-core`). Communicate via `mpsc` channels.
- **Tools**: Add new tools to `rc-tools`, then update the `ToolAction` union in `crates/rc-baml/baml_src/agent.baml` and the dispatcher in `crates/rc-core/src/agent.rs`.
