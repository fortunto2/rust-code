# CLAUDE.md — rust-code

AI-powered terminal coding agent written in Rust.

## Stack
- Rust (Edition 2024), Tokio async runtime
- Ratatui + Crossterm (TUI), tui-textarea (input)
- sgr-agent (LLM client + agent framework + session + tools + providers)
- Nucleo (fuzzy search, from Helix editor)
- rmcp (MCP client — Model Context Protocol)
- tmux (background task execution)

## Architecture
- `crates/rc-cli/` — main binary: TUI (app.rs), headless mode (main.rs), agent loop (agent.rs)
- `crates/sgr-agent/` — LLM client + agent framework + session/memory/tools/providers (all-in-one)
- `crates/sgr-agent-tui/` — shared TUI shell: chat panel, streaming, agent loop integration, fuzzy picker
- `crates/solograph/` — MCP server for code intelligence

Agent loop: user message → Agent::decide() → model returns `Decision { situation, task, tool_calls }` → execute tools → feed result back → repeat until `finish_task` or completion.

## sgr-agent Framework
- **LLM Client**: `GeminiClient`, `OpenAIClient` — structured output + function calling + flexible parse
- **Agent framework** (`feature = "agent"`):
  - `Tool` trait → `ToolRegistry` (builder, case-insensitive lookup, fuzzy resolve)
  - `Agent` trait → `Decision { situation, task, tool_calls, completed }`
  - 4 variants: `SgrAgent` (structured output), `ToolCallingAgent` (native FC), `FlexibleAgent` (text parse), `HybridAgent` (2-phase)
  - `run_loop()` — generic agent loop with 3-tier loop detection
  - `ToolFilter` — progressive discovery (keyword + fuzzy scoring)
- **Session** (`feature = "session"`): `Session`, `LoopDetector` (4-tier), `MemoryContext`, hints, tasks, intent guard
- **App tools** (`feature = "app-tools"`): bash, fs, git, apply_patch
- **Providers** (`feature = "providers"`): TOML config, auth, CLI proxy, Codex proxy
- **Telemetry** (`feature = "telemetry"`): OTEL file telemetry
- **Demo**: `cargo run -p sgr-agent --features agent --example agent_demo`
- **Tests**: `cargo test -p sgr-agent --all-features` — 410+ tests

## Agent Memory System
- **Agent home dir** (`.rust-code/`): SOUL.md, IDENTITY.md, MANIFESTO.md, RULES.md, MEMORY.md (user notes), MEMORY.jsonl (typed agent memory), context/*.md
- **Project context** (Claude Code compatible): AGENTS.md > CLAUDE.md > .claude/CLAUDE.md, with `@import` support
- **Rules**: `.agents/rules/*.md` > `.claude/rules/*.md`
- **MemoryTool**: agent writes typed JSONL entries (category, confidence, context)
- **GC**: tentative entries > 7 days auto-removed. Confirmed entries persist forever.
- **Token budget**: `to_system_message_with_budget()` drops low-priority parts first

## Development Rules
- TDD — write tests before implementing features
- Always run `make check` before committing (test + lint + fmt)
- Minimal changes — don't over-engineer
- app.rs is ~3000+ lines — be careful with edits, read before modifying
- Pre-commit hook enforces: tests, clippy (-D warnings on sgr-agent + sgr-agent-tui), fmt check
- Clippy gated on `sgr-agent` + `sgr-agent-tui` + `solograph` (rc-cli has legacy warnings)
- `cargo fmt` scoped to all crates

## Commands
```bash
make build           # dev build
make test            # run all tests (workspace)
make lint            # clippy on sgr-agent + sgr-agent-tui + solograph (-D warnings)
make fmt             # auto-format
make fmt-check       # format check (no write)
make check           # test + lint + fmt-check (pre-commit gate)
make release         # optimized release build
make install         # build + strip + install to /usr/local/bin
make audit           # unused deps + large files audit
make help            # show all targets
cargo run -- -p "prompt"  # test headless
```

## Release Process
```bash
# 1. Bump version in all Cargo.toml files
sed -i '' 's/version = "OLD"/version = "NEW"/' crates/*/Cargo.toml

# 2. Build macOS release locally
cargo build --release -p rust-code
strip target/release/rust-code

# 3. Package macOS
mkdir -p dist/rust-code-macos-aarch64
cp target/release/rust-code dist/rust-code-macos-aarch64/
cp README.md LICENSE dist/rust-code-macos-aarch64/
cd dist && tar czf ../rust-code-macos-aarch64.tar.gz rust-code-macos-aarch64 && cd ..
shasum -a 256 rust-code-macos-aarch64.tar.gz > rust-code-macos-aarch64.tar.gz.sha256

# 4. Commit, tag, push (triggers CI: Linux build + crates.io + Homebrew)
git add -A && git commit -m "release: vX.Y.Z"
git tag vX.Y.Z && git push origin master --tags

# 5. Upload macOS binary to release
gh release upload vX.Y.Z rust-code-macos-aarch64.tar.gz rust-code-macos-aarch64.tar.gz.sha256
```

## Key Files
| File | What |
|------|------|
| `crates/rc-cli/src/app.rs` | TUI — all panels, keybindings, drawing (~3k lines) |
| `crates/rc-cli/src/main.rs` | CLI entry, headless mode, sessions command |
| `crates/rc-cli/src/agent.rs` | Agent struct, 18 tools, SgrAgent impl |
| `crates/sgr-agent/src/lib.rs` | LLM client + agent framework + session exports |
| `crates/sgr-agent/src/agent.rs` | Agent trait, Decision, AgentError |
| `crates/sgr-agent/src/agent_tool.rs` | Tool trait, ToolOutput, ToolError |
| `crates/sgr-agent/src/registry.rs` | ToolRegistry (builder, lookup, fuzzy resolve) |
| `crates/sgr-agent/src/agent_loop.rs` | Generic agent loop + 3-tier loop detection |
| `crates/sgr-agent/src/agents/` | SgrAgent, ToolCallingAgent, FlexibleAgent, HybridAgent |
| `crates/sgr-agent/src/union_schema.rs` | Dynamic discriminated union schema builder |
| `crates/sgr-agent/src/client.rs` | LlmClient trait + Gemini/OpenAI impls |
| `crates/sgr-agent/src/discovery.rs` | ToolFilter (progressive discovery) |
| `crates/sgr-agent/src/gemini.rs` | GeminiClient (Google AI + Vertex AI) |
| `crates/sgr-agent/src/openai.rs` | OpenAIClient (OpenAI, OpenRouter, Ollama) |
| `crates/sgr-agent/src/flexible_parser.rs` | AnyOf cascade JSON parser (5 strategies) |
| `crates/sgr-agent/examples/agent_demo.rs` | Full 16-tool agent demo with Gemini |
| `crates/sgr-agent/src/session/` | Session module: traits, format, time, store, meta |
| `crates/sgr-agent/src/memory.rs` | MemoryContext, memory GC, token budget, @import |
| `crates/sgr-agent/src/loop_detect.rs` | 4-tier loop detection (exact, semantic, stagnation, frequency) |
| `crates/sgr-agent/src/app_loop.rs` | Session-based agent loop with streaming |
| `crates/sgr-agent/src/app_tools/` | Shared tools: bash, fs, git, apply_patch |
| `crates/sgr-agent/src/providers/` | Provider config, auth, CLI/Codex proxy |
| `crates/sgr-agent-tui/src/` | TUI shell: chat, picker, focus, command palette |
| `Makefile` | Build targets: check, lint, fmt, test, release, audit |
| `.githooks/pre-commit` | Pre-commit gate: test + clippy + fmt-check |
| `install.sh` | One-liner installer with doctor |
| `.github/workflows/release.yml` | CI: Linux build, crates.io, Homebrew update |

## Priorities (Roadmap)
| Priority | Task | Why |
|----------|------|-----|
| ~~P0~~ | ~~Streaming responses~~ | Done — streaming in TUI + headless |
| ~~P0~~ | ~~Context window management~~ | Done — 60-msg sliding window, system msgs preserved |
| ~~P1~~ | ~~Tests (TDD)~~ | Done — 410+ tests (sgr-agent) + 24 (rc-cli) |
| ~~P1~~ | ~~Agent framework~~ | Done — sgr-agent: Tool/Agent traits, Registry, 4 agent variants, loop |
| ~~P1~~ | ~~Merge baml-agent → sgr-agent~~ | Done — all modules consolidated, BAML removed |
| P1 | macOS CI (self-hosted runner) | Stop building manually |
| ~~P2~~ | ~~Multi-tool per step~~ | Done — `actions[]` array, parallel tool execution |
| P2 | Image/clipboard in chat | Paste screenshots for debugging |

## LLM Config
- Primary: Gemini 3.1 Pro Preview (best structured output)
- Fallback: Gemini 2.5 Flash → Gemini 3.1 Flash Lite
- Client: `AgentFallback` (auto-failover chain)
- Retry: exponential backoff, 3 retries, 500ms → 10s
- API key: `GEMINI_API_KEY` env var
