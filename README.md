# rust-code 🦀🤖

An AI-powered TUI coding agent written in Rust, leveraging Schema-Guided Reasoning (SGR) via BAML.

## Features
- 🚀 **Fast TUI**: Built with `ratatui` and `crossterm`.
- 🧠 **Schema-Guided Reasoning**: Uses `BAML` to strictly type LLM interactions and tool calling.
- 🔍 **Fuzzy Search**: Integrated with `nucleo` for blazingly fast file/code search.

## Architecture
- `rc-cli`: Terminal UI (Ratatui) and entry point.
- `rc-core`: Agent orchestration and SGR loop.
- `rc-tools`: Agent capabilities (fs, bash).
- `rc-baml`: BAML schemas and LLM clients.
