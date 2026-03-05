# rust-code

`rust-code` is a terminal coding agent written in Rust.

It combines a Ratatui-based TUI, typed tool execution, fuzzy navigation, session history, and a BAML-driven agent loop so you can work on a codebase without leaving the terminal.

## Install

From crates.io:

```bash
cargo install rust-code
```

Run it:

```bash
rust-code
```

Headless mode:

```bash
rust-code --prompt "Find the bug in src/main.rs"
rust-code --prompt "Summarize this repo" --resume
```

## Features

- Interactive terminal chat UI built with `ratatui` and `crossterm`
- Typed agent loop powered by BAML
- File read/write/edit tools
- Shell command execution
- Git status, diff, add, and commit tools
- Fuzzy file search with `nucleo`
- Session persistence in `.rust-code/session_*.jsonl`
- Session search and restore
- Git diff and git history side channels
- Project symbol search
- Background task / `tmux` session viewer
- Open-in-editor actions through `$EDITOR`

## Provider Setup

The current build is configured for these LLM backends:

- Google AI via `GEMINI_API_KEY`
- Vertex AI via `GOOGLE_CLOUD_PROJECT`
- OpenRouter via `OPENROUTER_API_KEY`

At least one of them must be configured in your environment before launching `rust-code`.

Examples:

```bash
export GEMINI_API_KEY="..."
rust-code
```

```bash
export GOOGLE_CLOUD_PROJECT="my-project"
rust-code
```

```bash
export OPENROUTER_API_KEY="..."
rust-code
```

Notes:

- `rust-code` currently initializes BAML clients that are defined in `crates/rc-baml/baml_src/clients.baml`.
- The checked-in config currently includes Gemini, Vertex AI, and OpenRouter.
- `BAML_LOG` is suppressed automatically by the app so the TUI stays clean.

## Quick Start

1. `cd` into the repository you want to work on.
2. Create an `AGENTS.md` file in that repo.
3. Export one provider credential.
4. Launch `rust-code`.
5. Start with a direct task like `review this repo`, `fix the failing test`, or `add a new command`.

## AGENTS.md

`rust-code` works best when the target repository contains an `AGENTS.md` file with project-specific instructions.

Recommended contents:

- stack and framework versions
- architecture constraints
- code style rules
- test/build commands
- migration or release rules
- prompt or tool-schema rules
- file locations that must be edited first

Example:

```md
# Agent Instructions

## Stack
- Rust 2024
- Tokio
- Ratatui

## Rules
- Prefer minimal patches
- Run `cargo check` after code changes
- Do not edit generated files directly
- Put prompt/schema changes under `crates/rc-baml/baml_src/`

## Commands
- Build: `cargo build`
- Check: `cargo check`
- Test: `cargo test`
```

The more concrete this file is, the better the agent performs.

## Sessions and Local State

`rust-code` stores local state in `.rust-code/`:

- `.rust-code/context/` for persistent agent guidance files
- `.rust-code/session_*.jsonl` for chat/session history

Use `--resume` to reopen the latest saved session:

```bash
rust-code --resume
```

## TUI Shortcuts

Main shortcuts currently exposed by the UI:

- `Enter`: send message
- `Ctrl+P`: file search
- `Ctrl+H`: session history
- `Ctrl+G`: refresh git sidebar
- `Tab`: focus sidebar
- `Ctrl+C`: quit
- `F1`: diff channel
- `F2`: git history
- `F3`: files
- `F4`: sessions
- `F5`: refresh
- `F6`: symbols
- `F7`: background tasks
- `F10`: channels
- `F12`: quit

Inside side panels:

- `Esc`: close panel
- `Ctrl+I`: insert selected item into the prompt
- `Ctrl+O`: open or attach, where supported

Background tasks are backed by `tmux`, so having `tmux` installed is useful if you want long-running task inspection from the UI.

## CLI

```text
Usage: rust-code [OPTIONS]

Options:
  -p, --prompt <PROMPT>
  -r, --resume
  -h, --help
  -V, --version
```

## Development

This repository now publishes a single crate, `rust-code`, but it still keeps a logical split in the source tree for the agent loop, tools, and generated BAML client code.

If you change BAML source files, edit them in:

- `crates/rc-baml/baml_src/`

Then regenerate:

```bash
cd crates/rc-baml
npx @boundaryml/baml@0.218.0 generate
```

Useful commands:

```bash
cargo check
cargo build
cargo test
```

## Status

The crate is published on crates.io:

- https://crates.io/crates/rust-code
