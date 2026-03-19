---
name: delegate
description: Orchestrate multiple CLI agents in parallel — delegate tasks to claude/gemini/codex/opencode/rust-code via .tasks/ system. Use when "delegate to", "spawn agents", "parallel analysis", "send task to claude", "run in background", "orchestrate agents". Do NOT use for in-process sub-agents (use spawn_agent) or single bash commands (use bash_bg).
allowed-tools: delegate_task, delegate_status, delegate_result, task, bash, read_file, finish
argument-hint: "<task description or .tasks/ path>"
---

# /delegate — Multi-Agent Orchestration

You are the orchestrator. You run on a cheap/fast model. You delegate complex work to powerful CLI agents and collect results.

## Available Agents

| Agent | Best for | Speed | Cost |
|-------|----------|-------|------|
| claude | Complex code changes, refactoring, multi-file edits | Slow | $$$ |
| gemini | Analysis, architecture review, large context | Fast | $ |
| codex | Focused code tasks, single-file fixes | Medium | $$ |
| opencode | Multi-model tasks (uses its own model config) | Medium | $$ |
| rust-code | Autonomous loops (BigHead mode, --loop 5) | Slow | $ |

Reference: `references/agent-guide.md`

## Two Modes

### 1. Free-text task
```
delegate_task {agent: "claude", task: "fix the auth bug in src/auth.rs"}
```

### 2. Task file (preferred for tracking)
```
task {operation: "create", title: "fix auth bug", priority: "high"}
delegate_task {agent: "claude", task_path: ".tasks/005-fix-auth-bug.md"}
```

The agent reads the task file, executes, updates `status: done`, writes `## Results`.

## Workflow

1. **Create tasks** in .tasks/ for each unit of work
2. **Delegate** to the right agent for each task
3. **Monitor** with `delegate_status` (check every 30-60s)
4. **Collect** results with `delegate_result` when done
5. **Review** — read .tasks/ files to see what each agent did

## Gotchas

1. **Agents inherit CLAUDE.md automatically** — don't paste project conventions into the task. Just point to the task file, the agent reads CLAUDE.md on its own.
2. **Check availability first** — `delegate_task` checks if the CLI is installed, but auth issues (expired tokens, missing API keys) only surface at runtime. If a delegate fails immediately, check tmux logs.
3. **task_path > free-text** — with task_path, the agent updates the file with results. With free-text, results are only in the tmux buffer (ephemeral). Always prefer task files for anything important.
4. **Don't poll too fast** — delegates are multi-minute processes. Poll every 30-60s, not every 5s. Use `bash {command: "sleep 30"}` between checks.
5. **Gemini CLI adds noise** — gemini-cli prints warnings about API keys to stderr. The actual analysis is in the output, ignore the warnings.

## Parallel Analysis Pattern

For code review / refactoring analysis, spawn 3 agents with different angles:

```
task {operation: "create", title: "code quality analysis", priority: "high"}
task {operation: "create", title: "architecture analysis", priority: "high"}
task {operation: "create", title: "performance analysis", priority: "high"}

delegate_task {agent: "claude", task_path: ".tasks/001-code-quality-analysis.md"}
delegate_task {agent: "gemini", task_path: ".tasks/002-architecture-analysis.md"}
delegate_task {agent: "opencode", task_path: ".tasks/003-performance-analysis.md"}
```

Then wait, collect results, synthesize, and delegate fixes.
