---
name: swarm-improve
description: Improve an external project with a swarm of agents — study, plan, branch, delegate to claude/codex, review, merge. Use when "improve project", "swarm improve", "send agents to fix", "refactor project X", "upgrade project". Do NOT use for single tasks (use /delegate) or own codebase (use /self-evolve).
allowed-tools: delegate_task, delegate_status, delegate_result, task, bash, read_file, search_code, git_status, git_diff, finish
argument-hint: "<project path>"
---

# /swarm-improve — Multi-Agent Project Improvement

Orchestrate a swarm of agents to study and improve an external project.
You are the architect. You plan, agents execute.

## Phase 1: Recon (you do this yourself, do NOT delegate)

1. `cd` to the project via bash
2. Read: `CLAUDE.md` or `README.md` → understand stack, conventions, commands
3. Read: `Makefile` or `package.json` → build/test commands
4. Quick scan: `bash: find . -name "*.rs" -o -name "*.ts" | head -30` (structure)
5. Check git: `bash: git log --oneline -10` (recent work)
6. Check tests: `bash: make test` or equivalent (baseline — must pass before changes)

## Phase 2: Branch

```
bash: cd <project> && git checkout -b improve/swarm-$(date +%Y%m%d)
```

All delegate work happens on this branch. Master stays clean.

## Phase 3: Plan & Create Tasks

Based on recon, create 3-5 focused tasks in the project's `.tasks/`:

```
task {operation: "create", title: "fix: <specific issue>", priority: "high"}
task {operation: "create", title: "refactor: <specific area>", priority: "medium"}
task {operation: "create", title: "test: <missing coverage>", priority: "medium"}
```

Keep tasks small and independent — each delegate gets ONE task.

## Phase 4: Delegate

Assign agents by strength:

| Task type | Agent | Why |
|-----------|-------|-----|
| Code changes, refactoring | claude | Best at multi-file edits |
| Review, analysis | codex | Good reviewer, free with subscription |
| Tests, docs | claude | Thorough, follows conventions |

```
delegate_task {agent: "claude", task_path: ".tasks/001-fix-issue.md", cwd: "<project>"}
delegate_task {agent: "claude", task_path: ".tasks/002-refactor.md", cwd: "<project>"}
delegate_task {agent: "codex", task_path: ".tasks/003-review.md", cwd: "<project>"}
```

## Phase 5: Monitor & Collect

Poll every 60s until all done:
```
bash: sleep 60
delegate_status
```

When done, read each task file for results:
```
read_file: <project>/.tasks/001-fix-issue.md
```

## Phase 6: Verify & Report

1. `bash: cd <project> && make check` (or equivalent test command)
2. `bash: git log --oneline improve/swarm-*..HEAD` (what was committed)
3. `bash: git diff --stat main..HEAD` (scope of changes)
4. Report: what was improved, what tests pass, ready for review

## Gotchas

1. **Always branch first** — never delegate to agents on master. Create `improve/swarm-YYYYMMDD`.
2. **Recon before delegation** — don't delegate blind. Read CLAUDE.md, check tests pass. Agents need working baseline.
3. **One task per delegate** — don't give one agent 5 tasks. Create separate tasks, assign separately. Parallel is faster.
4. **Check conflicts** — if two agents edit the same file, there will be merge conflicts. Assign non-overlapping files.
5. **codex may not be available** — subscription can expire. If delegate_task fails for codex, fall back to claude.
6. **Verify tests at the end** — agents may break things. Always run full test suite on the branch before reporting success.
