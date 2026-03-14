---
name: self-evolve
description: Self-evolution mode — analyze past sessions, find recurring issues, patch own code, test, commit, rebuild. Triggered by --evolve flag.
---

# Self-Evolution

You are in **self-evolution mode**. Your job is to improve yourself by analyzing your own performance history and fixing recurring issues.

## Process

### 1. Analyze History

Read your evolution log and recent sessions:

```
read_file: .rust-code/evolution.jsonl
```

Look at the last 5-10 entries. Identify:
- Declining scores (score_after < score_before)
- Repeated "discard" status
- High error counts or loop warnings

Then scan recent session logs for patterns:
```
bash: ls -t .rust-code/session_*.jsonl | head -5
```

Read the most recent sessions. Search for:
- `apply_patch error` — patch matching failures
- `Loop detected` — agent stuck in loops
- `Commit FAILED` — pre-commit hook failures
- `RE-READ` — wasteful file re-reads
- `Missing required parameter` — tool argument issues

### 2. Diagnose Root Cause

For each recurring issue, trace back to the source:
- **Patch failures**: Read `crates/sgr-agent/src/app_tools/apply_patch.rs` — is matching too strict?
- **Loop issues**: Read `crates/sgr-agent/src/loop_detect.rs` — are thresholds wrong?
- **Prompt issues**: Read `crates/rc-cli/src/agent.rs` — is system prompt unclear?
- **Tool errors**: Read the tool handler in `crates/rc-cli/src/agent.rs` — missing validation?

### 3. Fix (Minimal Change)

Apply the **simplest possible fix**. Follow the simplicity criterion:
- A 0.001 improvement that adds 20 lines of hacky code? NOT worth it.
- A 0.001 improvement from deleting code? Keep.
- Same performance but simpler code? Keep.

### 4. Test

```
bash: make check
```

All tests must pass. If not, fix and retry.

### 5. Commit

```
git_add: [changed files]
git_commit: "fix: [what you improved] (self-evolve)"
```

If commit fails (pre-commit hook), read the error, fix it, retry.

### 6. Score

After committing, compare your changes against baseline:
- If this is a code change to your own agent, include `RESTART_AGENT` in your finish summary
- The loop engine will rebuild and restart with the new binary

## Rules

- **ONE improvement per iteration**. Don't try to fix everything at once.
- **Always commit before restart**. Uncommitted changes are lost.
- **Never skip tests**. `make check` is your safety net.
- **Measure, don't guess**. Read actual session logs, don't assume.
- **Simplicity wins**. Remove code > add code. Fewer lines = fewer bugs.

## Signals

When done with this evolution cycle:
- Fixed something → `finish: "RESTART_AGENT — [what you fixed]"`
- Nothing to fix → `finish: "<solo:done/> — agent is clean, no improvements needed"`
- Stuck → `finish: "<solo:done/> — could not improve, need human input"`
