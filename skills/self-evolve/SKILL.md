---
name: self-evolve
description: Self-improvement mode — analyze past sessions for recurring failures, patch own code, test, commit, rebuild. Use when "evolve", "self-improve", "fix yourself", "analyze sessions", "--evolve". Do NOT use for normal coding (use /bighead) or delegating tasks (use /delegate).
allowed-tools: read_file, write_file, edit_file, apply_patch, bash, search_code, git_status, git_diff, git_add, git_commit, task, finish
argument-hint: "[focus area: patches|loops|prompts|tools]"
---

# Self-Evolution

Analyze your own performance history and fix recurring issues. ONE improvement per iteration.

## Process

1. Read `.rust-code/evolution.jsonl` + recent session logs
2. Find patterns: patch failures, loop warnings, error spikes
3. Trace to root cause in source code
4. Apply minimal fix, `make check`, commit
5. Signal `RESTART_AGENT` if you changed agent code

Reference files for diagnosis: `references/diagnosis-paths.md`

## Key Source Files

| Symptom | Source |
|---------|--------|
| Patch matching failures | `crates/sgr-agent/src/app_tools/apply_patch.rs` |
| Agent stuck in loops | `crates/sgr-agent/src/loop_detect.rs` |
| Prompt confusion | `crates/rc-cli/src/agent.rs` (SGR_SYSTEM_PROMPT) |
| Tool argument errors | `crates/rc-cli/src/agent.rs` (execute_action) |
| Schema issues | `crates/sgr-agent/src/schema.rs` |

## Signals

- Fixed something → `finish: "RESTART_AGENT — [what you fixed]"`
- Nothing to fix → `finish: "<solo:done/>"`
- Stuck → `finish: "<solo:done/> — need human input"`

## Gotchas

1. **RESTART_AGENT kills your context** — everything in memory is lost. Commit first, write notes to `.tasks/` for the next session to pick up.
2. **Simplicity threshold** — a 0.001 score improvement that adds 20 lines of hacky code is NOT worth it. Removing code with equal results is always a win.
3. **`make check` can take 2+ minutes** — don't call it in a tight loop. Run once after your change, fix issues, run once more to verify.
4. **Don't optimize what you can't measure** — read actual session logs, grep for error patterns. Don't guess at improvements.
5. **Evolution log is append-only** — don't edit `evolution.jsonl`. The loop engine reads it for scoring trends.
