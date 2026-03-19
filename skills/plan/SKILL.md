---
name: plan
description: Universal planning — recon any goal, delegate 3+ diverse agents for analysis, synthesize STAR-formatted tasks. Use when "plan", "create plan", "what should we do", "analyze and plan", "break this down", "plan improvements", "plan feature". Do NOT use for execution (use /swarm-improve or /delegate) or own codebase only (use /self-evolve).
allowed-tools: delegate_task, delegate_status, delegate_result, task, bash, read_file, search_code, git_status, finish
argument-hint: "<goal or project path>"
---

# /plan — Universal Multi-Agent Planning

Understand any goal, delegate analysis to 3+ diverse agents, synthesize into STAR-formatted `.tasks/`.

Works for anything: improve a project, build a feature, fix a bug, migrate systems, refactor architecture.

## STAR Task Model

Every execution task created by this skill follows STAR:

- **Situation** — current state, context, what exists now, why this matters
- **Task** — what needs to be done, acceptance criteria, scope boundary
- **Action** — specific steps, files to touch, approach, commands to run
- **Result** — expected outcome, how to verify success, what "done" looks like

## Phase 1: Recon (you do this — fast, no delegation)

Understand the Situation before planning anything.

1. Parse `$ARGUMENTS` — determine the goal:
   - Project path → cd there, study the project
   - Feature description → find the relevant codebase
   - Bug report → locate the affected area
   - Abstract goal → clarify scope with the user
2. Read: `CLAUDE.md`, `README.md` → stack, conventions, build commands
3. Structure scan: `bash: find . -type f \( -name "*.rs" -o -name "*.ts" -o -name "*.py" \) | head -40`
4. Git context: `bash: git log --oneline -15`
5. Baseline: run tests (must pass — if broken, that's the first task)
6. Existing work: `bash: ls .tasks/ 2>/dev/null` — don't duplicate

**Output:** 3-5 line situation summary — what exists, what's the goal, what's the gap.

## Phase 2: Design Analysis Dimensions

Based on the goal, pick 3+ analysis angles. Don't hardcode — adapt to the situation.

**Examples by goal type:**

| Goal | Dimension 1 | Dimension 2 | Dimension 3 | Optional 4th |
|------|-------------|-------------|-------------|--------------|
| Improve project | Code quality | Architecture | Performance | Security |
| Build feature | Existing patterns | Integration points | Edge cases | UX impact |
| Fix bug | Root cause | Blast radius | Regression risk | Related issues |
| Migration | Current state map | Target architecture | Risk assessment | Rollback plan |
| Refactor | Coupling analysis | API surface | Test coverage | Migration path |

The key: **each dimension gives a different perspective** on the same goal. If two dimensions would produce similar analysis — merge them, pick a different third.

Create analysis tasks via the `task` tool. **Keep titles short** — slugs are truncated to 30 chars:
```
task {operation: "create", title: "analysis: code quality", priority: "high"}
task {operation: "create", title: "analysis: architecture", priority: "high"}
task {operation: "create", title: "analysis: performance", priority: "high"}
```

Files are named `YYYYMMDD-NNN-slug.md` automatically. **Never create task files manually** — always use the `task` tool so IDs and dates are correct.

Customize each task body with: situation summary from recon, specific questions for this dimension, build/test commands, instruction to write findings in `## Results`.

Reference: `references/analysis-prompts.md` for templates.

## Phase 3: Delegate to Diverse Agents

Use **at least 3 different agents** for diverse perspectives. Agent diversity > agent quality for analysis.

| Agent | Strengths | Best for |
|-------|-----------|----------|
| claude | Deep reasoning, multi-file, nuanced judgment | Complex analysis, edge cases, architecture |
| gemini | Fast, large context, pattern detection | Structure analysis, code scanning, surveys |
| codex | Focused, single-file, deterministic | Targeted investigation, specific code areas |
| opencode | Multi-model, fresh perspective | Alternative viewpoint, cross-checking |
| rust-code | Autonomous loops, tool-heavy | Deep exploration with many tool calls |

**Diversity rules:**
- Never assign all dimensions to the same agent — you lose the whole point
- Prefer at least 3 unique agents: e.g. claude + gemini + codex
- If only 2 agents available — use both, but acknowledge reduced diversity
- Each agent's training data bias is a feature for analysis, not a bug

**Before delegating:** check which agents are actually available. If `delegate_task` fails for an agent (auth, not installed), immediately try the next one. Don't waste time retrying the same broken agent.

```
delegate_task {agent: "claude", task_path: ".tasks/YYYYMMDD-001-analysis-quality.md", cwd: "<project>"}
delegate_task {agent: "gemini", task_path: ".tasks/YYYYMMDD-002-analysis-arch.md", cwd: "<project>"}
delegate_task {agent: "codex", task_path: ".tasks/YYYYMMDD-003-analysis-perf.md", cwd: "<project>"}
```

**cwd is critical** — always pass the absolute project path as `cwd`. Delegates operate in their own shell; relative paths break.

## Phase 4: Monitor

Poll every 60s. Analysts need 2-5 minutes.

```
bash: sleep 60
delegate_status
```

When all report `status: done`, proceed.

## Phase 5: Synthesize into STAR Tasks

Read all analysis results. Then:

1. **Cross-reference** — same finding from multiple agents = high confidence
2. **Deduplicate** — merge overlapping findings
3. **Prioritize** by impact × effort:
   - P0: blocking, security, broken functionality
   - P1: significant improvement, < 1hr effort
   - P2: nice-to-have, polish, optimization

4. **Create STAR execution tasks**:

```
task {operation: "create", title: "fix: <specific issue>", priority: "high"}
```

Every execution task body MUST follow STAR:

```markdown
## Situation
<What exists now, why this is a problem, evidence from analysis agents.
Cross-reference: which agents flagged this, confidence level.>

## Task
<What needs to be done. Acceptance criteria. Scope boundary — what NOT to touch.>

## Action
<Specific steps: files to modify, functions to change, commands to run.
Not "improve error handling" but "add timeout to fetch_data() in src/api.rs, wrap in Result">

## Result
<Expected outcome. Verification command. What "done" looks like.
"make test passes, no new warnings, fetch_data returns Err after 30s timeout">
```

5. **Write plan summary** to `.tasks/PLAN.md`:

```markdown
# Plan: <goal summary>

## Situation
<Project/context overview from recon>

## Goal
<What we're trying to achieve>

## Date: <YYYY-MM-DD>
## Baseline: <test status, current state>

## Analysis Sources
- .tasks/NNN-analysis-dim1.md (agent: claude) — <dimension>
- .tasks/NNN-analysis-dim2.md (agent: gemini) — <dimension>
- .tasks/NNN-analysis-dim3.md (agent: codex) — <dimension>

## Key Findings
<Cross-referenced high-confidence findings — what multiple agents agree on>

## Execution Tasks (prioritized)
1. [P0] .tasks/NNN-fix-xxx.md — S: <one line> T: <one line>
2. [P1] .tasks/NNN-refactor-xxx.md — S: <one line> T: <one line>
3. [P2] .tasks/NNN-test-xxx.md — S: <one line> T: <one line>

## Parallelization
<Which tasks can run concurrently, which depend on others>

## Suggested Agents
<Recommended agent per task, based on task type>

## Out of Scope
<Items found but deferred, and why>
```

6. Report the plan to the user with `finish`.

## Gotchas

1. **Adapt dimensions to the goal** — "quality/arch/perf" is ONE possible split for project improvement. A feature plan needs "patterns/integration/edge-cases". A migration needs "mapping/target/risks". Think first, don't copy defaults.
2. **3 agents minimum, diversity mandatory** — the value of multi-agent planning is diverse perspectives. Two claudes analyzing the same code give ~1.2x insight. Claude + gemini + codex give ~2.5x. Never all same agent. Real failure: an orchestrator sent all 3 tasks to claude because codex was unavailable — zero diversity, wasted money.
3. **STAR is non-negotiable for execution tasks** — analysis tasks can be free-form. But every execution task MUST have all 4 STAR sections. A task without Result can't be verified. A task without Action can't be delegated.
4. **Baseline MUST pass — this is a HARD STOP** — if tests fail before analysis, do NOT proceed to Phase 2. Create a single P0 task "fix broken tests" and finish. Real failure: an orchestrator planned 3 tasks on a project with broken tests, all delegates failed because the baseline was broken. Total waste.
5. **Don't mix analysis and execution** — this skill creates the PLAN. Execution is `/swarm-improve` or `/delegate`. Plan first, report, let user decide.
6. **Use the `task` tool, never create files manually** — task files are `YYYYMMDD-NNN-slug.md`. The `task` tool handles naming, IDs, dates, and slug truncation. Real failure: an orchestrator wrote task files via `write_file` with long names that didn't match what `delegate_task` expected — all 3 delegations failed with "file not found".
7. **Always pass absolute cwd to delegates** — delegates run in their own shell. If you `cd` in your bash but pass a relative `task_path`, the delegate won't find it. Always: `cwd: "/absolute/path/to/project"`.
8. **Short task titles** — slugs are truncated to 30 chars. "analysis: code quality" is good. "analysis: Address clippy warnings in va-agent and va-agent-io" creates an unparseable filename.
