# CLAUDE.md — rust-code

AI-powered terminal coding agent written in Rust.

## Stack
- Rust (Edition 2024), Tokio async runtime
- Ratatui + Crossterm (TUI), tui-textarea (input)
- BAML (Schema-Guided Reasoning — typed LLM prompts)
- Nucleo (fuzzy search, from Helix editor)
- rmcp (MCP client — Model Context Protocol)
- tmux (background task execution)

## Architecture
- `crates/rc-cli/` — main binary: TUI (app.rs), headless mode (main.rs), agent loop (agent.rs)
- `crates/rc-baml/` — BAML source files (.baml) and generated client
- `crates/rc-core/` — core types (unused, to be cleaned)
- `crates/rc-tools/` — tool implementations (unused, inlined in rc-cli)

Agent loop: user message → BAML `GetNextStep()` → model returns `NextStep { analysis, plan_updates, action }` → execute action → feed result back → repeat until `FinishTaskTool`.

## BAML Rules
- **All prompts and tool schemas** live in `crates/rc-baml/baml_src/`
- **Every tool class MUST have `tool_name` literal discriminator** — prevents model from picking wrong tool in 14-variant union
- After editing .baml: `~/.cargo/bin/baml-cli generate --from crates/rc-baml/baml_src`
- Then sync: `rm -rf crates/rc-cli/src/baml_client && cp -r crates/rc-baml/src/baml_client crates/rc-cli/src/baml_client`
- If union changes (add/remove tool), update Union name in agent.rs, main.rs, app.rs via sed
- See `crates/rc-baml/README.md` for full prompt writing guide

## Development Rules
- TDD — write tests before implementing features
- `cargo check` after every code change
- `cargo test` before committing
- Minimal changes — don't over-engineer
- Don't edit generated `baml_client/` files directly
- app.rs is ~3000+ lines — be careful with edits, read before modifying

## Commands
```bash
cargo check          # type check
cargo build          # dev build
cargo test           # run tests
cargo build --release -p rust-code  # release build
cargo run -- -p "prompt"            # test headless
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
| `crates/rc-cli/src/main.rs` | CLI entry, headless mode, doctor command |
| `crates/rc-cli/src/agent.rs` | Agent struct, tool execution, session persistence |
| `crates/rc-baml/baml_src/agent.baml` | Tool schemas, NextStep union, agent prompt |
| `crates/rc-baml/baml_src/clients.baml` | LLM providers, fallback chain, retry policy |
| `install.sh` | One-liner installer with doctor |
| `.github/workflows/release.yml` | CI: Linux build, crates.io, Homebrew update |

## Priorities (Roadmap)
| Priority | Task | Why |
|----------|------|-----|
| ~~P0~~ | ~~Streaming responses~~ | Done — BAML streaming in TUI + headless |
| ~~P0~~ | ~~Context window management~~ | Done — 60-msg sliding window, system msgs preserved |
| ~~P1~~ | ~~Tests (TDD)~~ | Done — 14 inline tests (agent, fs, git) |
| P1 | macOS CI (self-hosted runner) | Stop building manually |
| ~~P2~~ | ~~Multi-tool per step~~ | Done — `actions[]` array, parallel tool execution |
| P2 | Image/clipboard in chat | Paste screenshots for debugging |

## LLM Config
- Primary: Gemini 3.1 Pro Preview (best structured output)
- Fallback: Gemini 2.5 Flash → Gemini 3.1 Flash Lite
- Client: `AgentFallback` (auto-failover chain)
- Retry: exponential backoff, 3 retries, 500ms → 10s
- API key: `GEMINI_API_KEY` env var
