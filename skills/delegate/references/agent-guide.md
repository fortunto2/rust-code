# Agent CLI Reference

## Claude Code
```bash
claude -p "task" --output-format json --dangerously-skip-permissions --verbose
```
- **Auth**: `~/.claude/` keychain (oauth)
- **Reads**: CLAUDE.md, .claude/rules/
- **Tools**: Read, Write, Edit, Bash, Grep, Glob — full file access
- **Output**: JSON with `{"type":"result","result":"..."}` on last line
- **Env**: Set `CLAUDECODE=''` to allow nesting inside another claude session
- **Cost**: ~$0.05-0.50 per task depending on complexity

## Gemini CLI
```bash
gemini -p "task" --sandbox -y
```
- **Auth**: `GEMINI_API_KEY` or `GOOGLE_API_KEY` env var
- **Reads**: GEMINI.md (project context)
- **Tools**: File read/write, bash (sandbox mode)
- **Output**: Plain text, may include JSON envelope with stats
- **Noise**: Prints "Both GOOGLE_API_KEY and GEMINI_API_KEY are set" — ignore
- **Cost**: ~$0.01-0.05 per task

## Codex CLI
```bash
codex exec "task" --dangerously-bypass-approvals-and-sandbox
```
- **Auth**: `~/.codex/auth.json` (ChatGPT Plus/Pro subscription)
- **Reads**: AGENTS.md
- **Tools**: File read/write, bash
- **Output**: Plain text
- **Cost**: Subscription-based (no per-call cost)

## OpenCode
```bash
opencode run "task" --format json
```
- **Auth**: Provider-specific (configured via `opencode providers`)
- **Reads**: Uses its own config for model selection
- **Model**: `-m provider/model` flag to override (e.g. `-m anthropic/claude-sonnet-4-20250514`)
- **Tools**: File read/write, bash, MCP servers
- **Output**: JSON events stream, last event has result
- **Cost**: Depends on configured provider/model

## rust-code (self-delegation)
```bash
rust-code -p "task" --loop 5
```
- **Auth**: `GEMINI_API_KEY` or configured provider
- **Reads**: CLAUDE.md, .rust-code/ agent home
- **Tools**: 25 built-in tools + MCP servers
- **BigHead mode**: `--loop N` runs autonomous iteration loop
- **Output**: Agent logs with [DONE] marker
- **Cost**: ~$0.02 per loop iteration (Gemini Flash)

## Choosing the Right Agent

| Scenario | Agent | Why |
|----------|-------|-----|
| Multi-file refactoring | claude | Best at complex code changes |
| Large codebase analysis | gemini | Fast, large context window |
| Quick single-file fix | codex | Free with subscription |
| Need specific model | opencode | Multi-model support |
| Autonomous multi-step | rust-code | BigHead loop with commit/test cycle |
| Parallel review | claude + gemini + opencode | Each reviews from different angle |

## Task File Protocol

When using `task_path`, the delegate agent is instructed to:

1. Read the task file (`.tasks/NNN-slug.md`)
2. Execute the task described in it
3. Update `status:` from `in_progress` to `done` in YAML frontmatter
4. Add `## Results` section with summary of what was done
5. Commit code changes if applicable

This means the orchestrator can just read the task file to get results — no need to parse tmux output.
