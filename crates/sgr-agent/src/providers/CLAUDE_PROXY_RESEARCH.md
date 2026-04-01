# Claude Subscription → API Proxy — Research Notes

> Date: 2026-04-01
> Status: Research complete, implementation pending
> Risk: Gray zone — see ToS section

## Goal

Use Claude Code Max/Pro subscription as LLM API backend (like `codex_proxy.rs` does for ChatGPT).

## What Works Now

| Approach | Tool calls | Models | Risk |
|----------|-----------|--------|------|
| `auth = "keychain"` + genai backend | Full | haiku only (sonnet/opus → 429) | Medium |
| `claude -p` CLI subprocess | None (text-only) | All (uses subscription) | Low |
| API key (`ANTHROPIC_API_KEY`) | Full | All | None (paid) |

### Keychain Auth (implemented in agent-bit)

```toml
# config.toml
[providers.claude]
model = "claude-haiku-4-5-20251001"
auth = "keychain"
```

- Token from macOS Keychain: `security find-generic-password -s "Claude Code-credentials" -w`
- Parsed: `json["claudeAiOauth"]["accessToken"]` → `sk-ant-oat01-...`
- Passed via `x-api-key` header through genai crate's Anthropic adapter
- **haiku**: works, full tool calls via Messages API
- **sonnet/opus**: 429 rate_limit_error (subscription tier limit on premium models)
- **Token expires ~8h** — needs refresh flow (not implemented yet)

### Benchmark: Claude Haiku 4.5 via Keychain

```
48.3% (14/29) — PAC1 dev benchmark
- Over-cautious: blocks legit tasks as DENIED_SECURITY (t01,t03,t13,t14,t17,t24)
- Token expired mid-bench: 401 on last 4 tasks
- Compare: gpt-5.4 = 71.4%, nemotron-120b = 60%
```

## ToS Analysis (Reddit/HN Research)

### Official Anthropic Position

**Consumer ToS §3.7**: No automated access "except via Anthropic API Key or where we otherwise explicitly permit it."

**Feb 2026 update**: "Using OAuth tokens obtained through Claude Free, Pro, or Max accounts in any other product, tool, or service — including the Agent SDK — is not permitted."

**Thariq Shihipar** (Anthropic engineer, Jan 2026):
- "Third-party harnesses using Claude subscriptions are prohibited by our ToS"
- "Anthropic will not be canceling accounts" for past usage
- They deployed server-side blocks returning: "This credential is only authorized for use with Claude Code"

### Risk Tiers

| Approach | Status | Risk |
|----------|--------|------|
| `claude -p` subprocess (official CLI) | **Allowed** | Low |
| `claude -p` wrapped as local API proxy | **Gray zone** | Medium |
| OAuth token via `x-api-key` to Messages API | **Blocked** | High |
| Spoofing Claude Code headers | **Banned** | Very high — Anthropic sent lawyers to OpenCode |

### Key Warning

If `ANTHROPIC_API_KEY` is set in env, `claude -p` **bills to API account** not subscription!
One user got $1,800+ bill in 2 days (GitHub #37686).

## Existing Proxies (GitHub)

### For using subscription as API:

| Project | Stars | Lang | Approach | Tool calls |
|---------|-------|------|----------|-----------|
| thhuang/claude-max-api-proxy-rs | 3 | Rust | CLI subprocess, dual-protocol | No |
| GodYeh/claude-max-api-proxy | 7 | TS | CLI subprocess, OpenAI-compat | No |
| horselock/claude-code-proxy | 111 | JS | Raw OAuth, Anthropic-native | No |

### Our existing code:

- `codex_proxy.rs` — ChatGPT Plus/Pro → Chat Completions (full proxy, token refresh)
- `cli_proxy.rs` — `claude -p` subprocess → text-only OpenAI-compat endpoint
- `auth.rs` — `load_claude_keychain_token()` from macOS Keychain

## Implementation Plan: `claude_proxy.rs`

Similar to `codex_proxy.rs` but for Claude subscription. Two options:

### Option A: CLI subprocess proxy (safe)

Like `cli_proxy.rs` but with streaming NDJSON parsing:
- Spawn `claude --print --output-format stream-json --no-session-persistence`
- Parse NDJSON events from stdout
- Convert to Chat Completions SSE response
- **Limitation**: no tool_call passthrough (Claude tools run internally)
- **Use case**: rc-cli, BAML, non-agent workloads

### Option B: Direct Messages API with token refresh (risky)

Like current keychain auth but with refresh flow:
- Load OAuth token from Keychain
- Refresh via `POST https://console.anthropic.com/api/oauth/token`
  - `grant_type=refresh_token`
  - `refresh_token=sk-ant-ort01-...`
  - `client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e` (Claude Code's OAuth client ID)
- Full tool calls via genai Anthropic adapter
- **Limitation**: sonnet/opus get 429; only haiku works
- **Risk**: Explicitly against ToS since Feb 2026

### Recommendation

Option A for general use (safe, text-only).
For PAC1 agent with tool calls: use API key for sonnet/opus, keychain for haiku only.

## Token Refresh Flow (for reference)

```
POST https://console.anthropic.com/api/oauth/token
Content-Type: application/x-www-form-urlencoded

grant_type=refresh_token
&refresh_token=sk-ant-ort01-...
&client_id=9d1c250a-e61b-44d9-88ed-5944d1962f5e
```

Response: `{ "access_token": "sk-ant-oat01-...", "refresh_token": "sk-ant-ort01-...", "expires_in": 28800 }`

The `client_id` is Claude Code CLI's registered OAuth client — may change between versions.

## Files Changed

- `sgr-agent/src/types.rs` — added `use_genai: bool` to `LlmConfig`
- `sgr-agent/src/llm.rs` — explicit genai routing when `use_genai = true`
- `agent-bit/Cargo.toml` — added `genai` + `providers` features
- `agent-bit/config.toml` — `[providers.claude]` and `[providers.claude-haiku]` with `auth = "keychain"`
- `agent-bit/src/config.rs` — `auth` field, keychain resolution
- `agent-bit/src/main.rs` — `use_genai` flag for claude/gemini models
