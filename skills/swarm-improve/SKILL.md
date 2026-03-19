---
name: swarm-improve
description: End-to-end project improvement — plan, branch, delegate execution, verify. Calls /plan for analysis, then delegates fixes to agents. Use when "improve project", "swarm improve", "send agents to fix", "refactor project X", "upgrade project". Do NOT use for planning only (use /plan), single tasks (use /delegate), or own codebase (use /self-evolve).
allowed-tools: delegate_task, delegate_status, delegate_result, task, bash, read_file, search_code, git_status, git_diff, finish
argument-hint: "<project path>"
---

# /swarm-improve — Multi-Agent Project Improvement

End-to-end workflow: plan → branch → delegate execution → verify.

This is a thin orchestrator. The heavy lifting is in `/plan` (analysis) and `/delegate` (execution).

## Step 0: Validate Baseline

**Before anything else:**
1. `cd` to the project (absolute path — store it, you'll need it for every delegate)
2. Read `CLAUDE.md` or `README.md` → find the test command
3. Run tests: `bash: make test` (or equivalent)

**If tests FAIL → HARD STOP.** Create one P0 task "fix broken tests" and finish. Do NOT plan improvements on a broken project — all delegate work will fail too.

## Step 1: Plan

If `.tasks/PLAN.md` already exists and is recent (< 24h):
- Read it, skip to Step 2
- Tell the user you're using the existing plan

Otherwise, run the `/plan` workflow inline (this skill has the same tools):

1. **Recon** — already done in Step 0
2. **Design dimensions** — pick 3+ analysis angles adapted to the goal (not hardcoded)
3. **Check agent availability** — try each agent before committing to the plan
4. **Delegate to 3+ diverse agents** — agent diversity mandatory (claude + gemini + codex preferred)
5. **Wait** — poll every 60s with `delegate_status`
6. **Synthesize** — cross-reference, dedup, create STAR-formatted execution tasks + `.tasks/PLAN.md`

**Use the `task` tool** for all task creation — never write .tasks/ files manually. Files are named `YYYYMMDD-NNN-slug.md` automatically.

## Step 2: Branch

```
bash: cd <absolute-project-path> && git checkout -b improve/swarm-$(date +%Y%m%d)
```

All delegate work happens on this branch. Master/main stays clean.

## Step 3: Delegate Execution

Read `.tasks/PLAN.md` for the prioritized task list. Each task has STAR format — **Action** tells the delegate what to do, **Result** tells how to verify.

Assign agents by strength, **diversify across agents**:

| Task type | Primary | Fallback |
|-----------|---------|----------|
| Code changes, refactoring | claude | gemini |
| Quick fixes, single-file | codex | claude |
| Tests, docs | claude | codex |
| Review, analysis | gemini | opencode |

**Always pass absolute cwd:**
```
delegate_task {agent: "claude", task_path: ".tasks/YYYYMMDD-001-fix-xxx.md", cwd: "/absolute/path/to/project"}
delegate_task {agent: "gemini", task_path: ".tasks/YYYYMMDD-002-refactor-xxx.md", cwd: "/absolute/path/to/project"}
```

If an agent fails immediately (not installed, auth error) — switch to fallback agent, don't retry the broken one.

**Limit: 3 delegates at a time.** When one finishes, start the next.

## Step 4: Monitor & Collect

Poll every 60s:
```
bash: sleep 60
delegate_status
```

When a delegate finishes, read its task file for results:
```
read_file: <project>/.tasks/YYYYMMDD-001-fix-xxx.md
```

If a delegate failed — note it, move on. Don't retry in this cycle.

## Step 5: Verify

After all delegates finish:

1. `bash: cd <project> && make check` (or project's test command)
2. `bash: git log --oneline improve/swarm-*..HEAD` — what was committed
3. `bash: git diff --stat main..HEAD` — scope of changes

## Step 6: Report

```
finish: "Improvement complete on branch improve/swarm-YYYYMMDD.
- N tasks planned, M completed, K failed
- Agents used: claude, gemini, codex (diversity: N unique)
- Tests: pass/fail
- Files changed: N
- Ready for review: git diff main..improve/swarm-YYYYMMDD"
```

## Gotchas

1. **Baseline MUST pass — HARD STOP if broken** — real failure: orchestrator planned 3 tasks on a project with broken tests. All delegates failed because the code didn't compile. Total waste of time and API credits. Run `make test` FIRST.
2. **Always branch BEFORE delegating** — never delegate on master. If working tree is dirty, stash or commit first.
3. **3 delegates max concurrently** — agents compete for CPU, rate limits, API quota. When one finishes, start the next.
4. **Agent diversity on execution too** — don't send all tasks to claude. Mix agents: claude for complex edits, codex for simple fixes, gemini for analysis. Real failure: all 3 sent to claude = $0.50+ and no diversity.
5. **Absolute paths everywhere** — `cwd` must be absolute. Task paths in `delegate_task` are relative to cwd. Real failure: orchestrator cd'd in bash then used relative paths — delegates couldn't find task files.
6. **Use `task` tool, never `write_file` for tasks** — the `task` tool generates correct `YYYYMMDD-NNN-slug.md` names with proper IDs. Real failure: orchestrator created files via `write_file` with 60-char names, then `delegate_task` couldn't find them.
7. **Verify MUST run** — even if all tasks report success, run the full test suite. Agents may break each other's work through shared file edits.
8. **codex/gemini may not be available** — check early. If `delegate_task` fails for an agent, switch to fallback immediately. Don't waste time retrying broken agents.
