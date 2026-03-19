# Skill Quality Checklist

Based on Anthropic's internal skills best practices (https://x.com/trq212/status/2033949937936085378).

Score each dimension 0-2: 0 = missing, 1 = partial, 2 = good. Max score: 24.

---

## 1. Category Fit (0-2)

Does the skill fit cleanly into ONE category?

| Category | Signal |
|----------|--------|
| Library & API Reference | Explains how to use a library, CLI, SDK. Gotchas, snippets, edge cases |
| Product Verification | Tests/verifies output is correct. Playwright, simulator, assertions |
| Data Fetching & Analysis | Connects to data/monitoring stacks. Credentials, dashboard IDs, workflows |
| Business Process & Automation | Automates repetitive workflows. Logs previous results for consistency |
| Code Scaffolding & Templates | Generates boilerplate for specific codebase function. May include scripts |
| Code Quality & Review | Enforces code quality. Deterministic scripts for robustness |
| CI/CD & Deployment | Fetch, push, deploy. May reference other skills for data |
| Runbooks | Symptom → investigation → structured report |
| Infrastructure Operations | Routine maintenance with guardrails for destructive actions |

- **2** — fits one category cleanly
- **1** — mostly one but bleeds into another
- **0** — unclear category or straddles several

## 2. Description Quality (0-2)

The `description:` field is scanned by Claude to decide "is there a skill for this request?"

Check:
- [ ] Has 3+ trigger phrases (what user says to invoke)
- [ ] Has "Do NOT use for X (use /other)" negative examples
- [ ] Specific enough to avoid false triggers on adjacent skills
- [ ] Under 300 chars (long descriptions dilute signal)

- **2** — trigger phrases + negative examples + no false triggers
- **1** — has triggers but missing negatives, or too vague
- **0** — generic description that could match anything

## 3. Progressive Disclosure (0-2)

"A skill is a folder, not just a markdown file."

Check:
- [ ] Uses `references/` for detailed content (API docs, templates, command lists)
- [ ] SKILL.md tells Claude what reference files exist and when to read them
- [ ] Scripts in `scripts/` for automation
- [ ] Templates in `assets/` for output generation
- [ ] SKILL.md stays under 300 lines (core logic only)

- **2** — reference files exist, SKILL.md points to them, under 300 lines
- **1** — has references OR is short, but not both
- **0** — everything crammed into one SKILL.md, over 400 lines

## 4. Gotchas Section (0-2)

"The highest-signal content in any skill."

Check:
- [ ] Has explicit `## Gotchas` section
- [ ] 3-5 concrete pitfalls (not generic warnings)
- [ ] Each gotcha is from real failure experience (not hypothetical)
- [ ] Gotchas push Claude away from default behavior it would otherwise follow

- **2** — 3+ concrete, experience-based gotchas
- **1** — has "Common Issues" but not real gotchas (more troubleshooting than prevention)
- **0** — no gotchas or pitfalls section

## 5. Don't State the Obvious (0-2)

"Focus on information that pushes Claude out of its normal way of thinking."

Check:
- [ ] No generic coding advice Claude already knows
- [ ] No framework docs that Claude has in training data
- [ ] Focuses on YOUR codebase specifics, internal conventions, non-obvious patterns
- [ ] Gotchas are things Claude would get WRONG by default

- **2** — every section adds non-obvious value
- **1** — mix of obvious and non-obvious content
- **0** — mostly restates what Claude already knows

## 6. Flexibility vs Railroading (0-2)

"Give Claude the information it needs, but give it the flexibility to adapt."

Check:
- [ ] Gives principles and context, not rigid step-by-step scripts
- [ ] Allows Claude to skip/adapt steps based on situation
- [ ] Uses "if X then Y" branching, not linear railroad
- [ ] Doesn't over-specify output format for every case

- **2** — provides context + principles, Claude adapts to situation
- **1** — mostly flexible but some unnecessarily rigid sections
- **0** — strict step-by-step that breaks on edge cases

## 7. Setup & Config (0-2)

"Store setup information in a config.json file in the skill directory."

Check:
- [ ] Handles first-run setup gracefully (detects missing config)
- [ ] Asks user for config via AskUserQuestion when needed
- [ ] Stores config in stable location (not overwritten on skill update)
- [ ] Works with sensible defaults if config is missing

- **2** — proper config pattern with detection + fallback
- **1** — handles setup but no persistent config
- **0** — assumes everything is configured, crashes on first run
- **N/A** — skill doesn't need configuration (score as 2)

## 8. Memory & State (0-2)

"Some skills can include a form of memory by storing data within them."

Check:
- [ ] Stores results/logs for future reference when appropriate
- [ ] Previous runs inform current run (e.g., standup skill reads history)
- [ ] Uses stable storage path (not deleted on skill update)
- [ ] Avoids accumulating unbounded state

- **2** — meaningful state that improves over time
- **1** — stateless but could benefit from memory
- **0** — should store state but doesn't
- **N/A** — genuinely stateless skill (score as 2)

## 9. Scripts & Composable Code (0-2)

"Giving Claude scripts and libraries lets Claude spend its turns on composition."

Check:
- [ ] Includes scripts/ or code snippets Claude can execute
- [ ] Code is composable (functions, not monoliths)
- [ ] Claude can generate new scripts by combining provided building blocks
- [ ] Prefers giving Claude code over natural language instructions for complex operations

- **2** — includes scripts/snippets that Claude composes into solutions
- **1** — has some code but mostly instructions
- **0** — all natural language, no executable code
- **N/A** — skill doesn't involve code execution (score as 2)

## 10. Allowed Tools (0-2)

Check:
- [ ] `allowed-tools:` lists only what's actually needed
- [ ] MCP tools listed with "use if available" fallback pattern
- [ ] Doesn't request overly broad tool access

- **2** — minimal, correct tool list with MCP fallback
- **1** — mostly correct but includes unnecessary tools
- **0** — missing allowed-tools or requests everything

## 11. Argument Handling (0-2)

Check:
- [ ] Has `argument-hint:` in frontmatter
- [ ] Parses `$ARGUMENTS` with clear fallback (empty → ask user)
- [ ] Supports common input patterns (name, path, URL, flag)

- **2** — hint + parsing + graceful fallback
- **1** — has arguments but no fallback or hint
- **0** — ignores arguments entirely

## 12. Output & Artifacts (0-2)

Check:
- [ ] Writes output to a file (not just prints to console)
- [ ] Output path is predictable and documented
- [ ] Output format is useful for downstream skills/pipeline
- [ ] Includes summary for user (not just raw data dump)

- **2** — file output + summary + pipeline-compatible
- **1** — prints to console only, or outputs without summary
- **0** — unclear what the skill produces

---

## Scoring

| Score | Grade | Action |
|-------|-------|--------|
| 22-24 | A | Ship-ready. Minor polish only |
| 18-21 | B | Good. Fix 1-2 weak areas |
| 14-17 | C | Functional but needs work. Prioritize gotchas + progressive disclosure |
| 10-13 | D | Needs rewrite. Missing fundamentals |
| 0-9 | F | Stub or notes, not a skill |
