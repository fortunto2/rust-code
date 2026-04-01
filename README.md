# rust-code

[![Crates.io](https://img.shields.io/crates/v/rust-code)](https://crates.io/crates/rust-code)
[![GitHub Release](https://img.shields.io/github/v/release/fortunto2/rust-code)](https://github.com/fortunto2/rust-code/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

`rust-code` is a terminal coding agent written in Rust.

It combines a Ratatui-based TUI, typed tool execution, fuzzy navigation, session history, and an SGR-driven agent loop so you can work on a codebase without leaving the terminal.

![Fuzzy File Search with preview](docs/assets/screenshot-file-search.png)

<p align="center">
  <img src="docs/assets/screenshot-bg-tasks.png" width="32%" alt="BG Tasks — realtime tmux preview">
  <img src="docs/assets/screenshot-symbols.png" width="32%" alt="Project Symbols">
  <img src="docs/assets/screenshot-skills.png" width="32%" alt="Skills Browser">
</p>

## Install

Homebrew (macOS, auto-installs tmux):

```bash
brew install fortunto2/tap/rust-code
```

One-liner (downloads binary + installs dependencies via `doctor`):

```bash
curl -fsSL https://raw.githubusercontent.com/fortunto2/rust-code/master/install.sh | bash
```

From crates.io:

```bash
cargo install rust-code
rust-code doctor --fix   # installs tmux, ripgrep, etc.
```

Run it:

```bash
rust-code                                          # interactive TUI
rust-code -p "Find the bug in src/main.rs"         # headless mode
rust-code --resume                                 # continue last session
rust-code --resume "refactor"                      # fuzzy-search sessions by topic
rust-code -p "build feature X" --loop 5            # autonomous loop (BigHead mode)
rust-code -p "improve yourself" --evolve           # self-evolution mode
```

## Features

- **Interactive TUI** — chat UI built with `ratatui` and `crossterm`
- **SGR agent loop** — typed tool execution with fallback chain (Gemini Pro → Flash → Flash Lite)
- **22 built-in tools** — file read/write/edit/patch, bash (fg + bg), search, git, memory, tasks, agent swarm, MCP, OpenAPI
- **Agent swarm** — spawn child agents with roles, wait/cancel, parallel task execution
- **Task management** — persistent kanban board via `.tasks/*.md`
- **Fuzzy file search** (`Ctrl+P`) — fast file navigation with `nucleo` and live file preview
- **Project symbol search** (`F6`) — browse functions, structs, enums with code preview
- **Background tasks** (`F7`) — run long commands in `tmux` windows with realtime output preview
- **Skills system** (`F9`) — browse, search, and install agent skills from [skills.sh](https://skills.sh) registry
- **MCP support** — connect external tool servers via `.mcp.json` (e.g. Playwright, codegraph, Supabase)
- **OpenAPI → Tool** — any API as one tool: load spec → fuzzy search endpoints → call. 10 popular APIs pre-configured (GitHub, Cloudflare, Stripe, OpenAI, etc.) + APIs.guru directory (2800+ APIs). TOML registry at `~/.sgr-agent/apis.toml`
- **Git integration** — diff sidebar, history viewer, stage and commit from the agent
- **Session persistence** — chat history in `.rust-code/session_*.jsonl`, resume with `--resume`
- **Open-in-editor** — jump to file:line in `$EDITOR` from any panel
- **BigHead mode** (`--loop N`) — autonomous task loop with circuit breaker, control file, and `<solo:done/>` signal
- **Self-evolution** (`--evolve`) — agent evaluates its own runs, patches code, rebuilds, and restarts

### Background Tasks (tmux)

The agent can run long-lived commands (dev servers, watchers, builds) in named `tmux` windows via `BashBgTool`. Press `F7` to see all running tasks with realtime log output. `Ctrl+O` to attach, `Ctrl+K` to kill.

Requires `tmux` installed (`brew install tmux`).

### Skills

Skills are reusable agent instructions (markdown files) that teach the agent domain-specific workflows. Browse the [skills.sh](https://skills.sh) registry with `F9`, or from CLI:

```bash
rust-code skills search "deploy"
rust-code skills add tavily-ai/skills/web-search
```

Installed skills are injected into the agent context automatically.

### MCP (Model Context Protocol)

Connect external tool servers by adding `.mcp.json` in your project or home directory:

```json
{
  "mcpServers": {
    "playwright": {
      "command": "npx",
      "args": ["@playwright/mcp@latest"]
    }
  }
}
```

The agent discovers MCP tools at startup and can call them via `McpToolCall`.

## Setup & Providers

On first launch, `rust-code` runs an interactive `setup` wizard to configure your preferred LLM backends and verifies authentication using `rust-code doctor`.

You can also run these manually:

```bash
rust-code setup
rust-code doctor
```

Supported providers include:

- **Google AI** via `GEMINI_API_KEY`
- **Vertex AI** via `VERTEX_PROJECT` (uses Google Cloud ADC or service account)
- **Anthropic** via `ANTHROPIC_API_KEY`
- **OpenRouter** via `OPENROUTER_API_KEY`
- **Ollama** (local via `OLLAMA_HOST`)

At least one provider must be configured before launching `rust-code`.

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

- Default: Gemini 3.1 Pro Preview with fallback to Flash and Flash Lite.
- Provider configuration: `~/.rust-code/config.toml` or `rust-code setup` or `rust-code config set`.
- Vertex AI uses `global` region by default (not `us-central1`).

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
- Run `make check` before committing

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
Usage: rust-code [COMMAND] [OPTIONS]

Commands:
  setup       Interactive provider setup wizard
  doctor      Check system deps and API auth (--fix to auto-install)
  skills      Manage agent skills (add, remove, search, list, catalog)
  sessions    List or search past chat sessions
  mcp         Show MCP server status and tools
  config      Set default provider (show, set, reset)
  task        Manage project tasks (list, show, create, done, update)

Options:
  -p, --prompt <PROMPT>    Run in headless mode with a prompt
  -r, --resume [TOPIC]     Resume last session or fuzzy-search by topic
  -s, --session <PATH>     Resume specific session file
      --cwd <PATH>         Working directory for headless mode
      --model <NAME>       Override model name
      --intent <MODE>      Intent mode: auto, ask, build, plan
      --local              Use local Ollama model
      --codex              Use ChatGPT Plus/Pro via Codex proxy
      --gemini-cli         Use Gemini CLI as LLM backend
      --loop <N>           Autonomous loop (BigHead mode)
      --max-hours <FLOAT>  Time limit for loop/evolve mode
      --evolve             Self-evolution mode
  -h, --help               Print help
  -V, --version            Print version
```

### OpenAPI → Tool

Convert any REST API into a searchable, callable tool — no code generation needed.

```bash
# The agent can search and call APIs on the fly:
# "search github repos" → finds GET /search/repositories → calls it
```

- 10 popular APIs pre-configured: GitHub (1093 endpoints), Cloudflare (2656), Stripe, OpenAI, Supabase, PostHog, Slack, Linear, Vercel, Sentry
- APIs.guru fallback: 2800+ APIs searchable by name
- Auto-cache specs to `~/.sgr-agent/openapi-cache/`
- TOML registry at `~/.sgr-agent/apis.toml` for custom APIs
- Auto-detect auth from env vars (`GITHUB_TOKEN`, `STRIPE_SECRET_KEY`, etc.)
- Full `$ref` resolution, path-level parameter inheritance, YAML support

### BigHead Mode (Autonomous Loop)

Run the agent in a loop for autonomous task execution:

```bash
rust-code -p "build a CLI tool for air quality" --loop 10 --max-hours 2
```

- Circuit breaker: stops after 3 consecutive identical failures
- Control file: `.rust-code/loop-control` (write `stop`, `pause`, `skip`)
- Signal: agent outputs `<solo:done/>` when task is complete
- Skills: `--loop` auto-loads `skills/bighead/SKILL.md`

### Self-Evolution

The agent can evaluate and improve itself:

```bash
rust-code -p "improve your error handling" --evolve
```

- Evaluates each run: error rate, loop warnings, patch failures, steps
- Analyzes session history for recurring patterns
- Proposes and applies improvements to its own code
- Rebuilds and restarts via `RESTART_AGENT` signal
- Evolution log: `.rust-code/evolution.jsonl`

## Development

The workspace consists of 5 crates:

| Crate | What |
|-------|------|
| `rc-cli` | Main binary — TUI, headless mode, 22 tools, agent loop |
| `sgr-agent` | LLM client + agent framework + session/memory/tools/providers/OpenAPI |
| `sgr-agent-tui` | Shared TUI shell — chat panel, fuzzy picker, focus system |
| `solograph` | MCP server for code intelligence |
| `genai` | Local fork of rust-genai — multi-provider LLM client |

```bash
make build    # dev build
make test     # run all tests (450+ in sgr-agent alone)
make lint     # clippy on sgr-agent + sgr-agent-tui + solograph (-D warnings)
make check    # test + clippy + fmt (pre-commit gate)
make install  # build + strip + install to /usr/local/bin
make audit    # unused deps + large files audit
make help     # show all targets
```

## Built With

| What | Crate / Link |
|------|-------------|
| Agent architecture | [Schema-Guided Reasoning (SGR)](https://abdullin.com/schema-guided-reasoning/) — typed tool dispatch via union types |
| LLM client | [rust-genai](https://github.com/jeremychone/rust-genai) (local fork) — multi-provider Rust client (Gemini, OpenAI, Anthropic, Ollama, etc.) |
| TUI framework | [Ratatui](https://github.com/ratatui/ratatui) + [Crossterm](https://github.com/crossterm-rs/crossterm) |
| Text input | [tui-textarea](https://github.com/rhysd/tui-textarea) |
| Fuzzy search | [Nucleo](https://github.com/helix-editor/nucleo) (from Helix editor) |
| Async runtime | [Tokio](https://tokio.rs) |
| MCP client | [rmcp](https://github.com/modelcontextprotocol/rust-sdk) — Rust SDK for Model Context Protocol |
| CLI | [Clap](https://github.com/clap-rs/clap) |
| File traversal | [ignore](https://github.com/BurntSushi/ripgrep/tree/master/crates/ignore) (from ripgrep, respects `.gitignore`) |
| Skills registry | [skills.sh](https://skills.sh) |
| Background tasks | [tmux](https://github.com/tmux/tmux) |

## Status

The crate is published on crates.io:

- https://crates.io/crates/rust-code

Release artifacts (Linux x86_64 + macOS aarch64) are published on GitHub when you push a tag matching `v*`.

## Comparison with Other Terminal Agents

| | rust-code | Claude Code | [claw-code](https://github.com/instructkr/claw-code) | Codex CLI | Aider |
|---|---|---|---|---|---|
| **Language** | Rust | TypeScript | Rust | TypeScript | Python |
| **LLM providers** | Gemini, OpenAI, Anthropic, Vertex, Ollama, OpenRouter | Anthropic only | Anthropic (+ OpenAI via [openai-oxide](https://crates.io/crates/openai-oxide)) | OpenAI only | Multi-provider |
| **Agent variants** | 6 (structured, tool-calling, flexible, hybrid, planning, clarification) | 1 | 1 | 1 | 1 |
| **Loop detection** | 4-tier (exact, semantic, output stagnation, frequency churn) | Unknown | None | None | None |
| **Context compaction** | LLM-based + incremental | LLM-based | Heuristic (no LLM) | Sliding window | Repo map |
| **Memory** | JSONL typed entries + GC + token budget | File-based | None | None | Repo map |
| **Tools** | 27+ with fuzzy matching | 20+ | 19 | ~10 | Edit/shell |
| **MCP support** | Yes (rmcp) | Yes | Yes (stdio) | Yes | No |
| **OpenAPI → Tool** | 11 pre-configured APIs + APIs.guru (2800+) | No | No | No | No |
| **Background tasks** | Yes (tmux) | No | No | No | No |
| **Agent swarm** | Yes (multi-agent) | Yes (Agent tool) | Agent tool (stub) | No | No |
| **Self-evolution** | Yes (`--evolve`) | No | No | No | No |
| **Autonomous loop** | Yes (`--loop N`) | No | No | Yes | No |
| **TUI** | Full (ratatui) | Inline terminal | Inline REPL | Inline terminal | Inline terminal |
| **Session resume** | Yes (fuzzy search) | Yes | Yes | No | Yes |
| **Config layering** | Global → project → local → env → CLI | 5-level JSON merge | 2-level | Env only | YAML |
| **License** | MIT | Proprietary | MIT | Apache-2.0 | Apache-2.0 |
