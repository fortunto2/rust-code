---
name: solo-skill-audit
description: Audit a skill against Anthropic's best practices checklist — 12 dimensions, scored 0-24. Use when user says "audit skill", "review skill quality", "check skill", "skill score", "skill checklist", or "is this skill good". Do NOT use for KB audits (use /audit) or code review (use /review).
license: MIT
metadata:
  author: fortunto2
  version: "1.0.0"
  openclaw:
    emoji: "🔍"
allowed-tools: Read, Grep, Glob, Bash
argument-hint: "<skill-name or path>"
---

# /skill-audit

Audit a skill against the quality checklist based on Anthropic's internal best practices for Claude Code skills. Reads the skill's SKILL.md, references/, scripts/, and evaluates across 12 dimensions.

Source: https://x.com/trq212/status/2033949937936085378

## Checklist Reference

Full checklist with scoring rubric: `references/checklist.md`

## Steps

1. **Locate skill** from `$ARGUMENTS`:
   - If name: search `skills/{name}/SKILL.md` in current project, then `~/.claude/plugins/**/skills/{name}/SKILL.md`
   - If path: read directly
   - If empty: list available skills via Glob `**/skills/*/SKILL.md`, ask via AskUserQuestion

2. **Read skill contents** (parallel):
   - `SKILL.md` — main skill file
   - `references/*` — all reference files (if dir exists)
   - `scripts/*` — all scripts (if dir exists)
   - `assets/*` — all assets (if dir exists)
   - Count total lines of SKILL.md

3. **Read checklist** from `references/checklist.md`

4. **Evaluate each dimension** — for every checklist item, assess based on what you read:

   For each of the 12 dimensions:
   - State what you found (evidence)
   - Score 0, 1, or 2 (use rubric from checklist)
   - If score < 2: give one specific fix

5. **Determine category** — classify skill into one of 9 types from checklist. Flag if it straddles multiple.

6. **Output scorecard:**

   ```
   ## Skill Audit: {skill-name}

   **Score:** {N}/24 (Grade: {A/B/C/D/F})
   **Category:** {type}
   **SKILL.md:** {N} lines | **References:** {N} files | **Scripts:** {N} files

   | # | Dimension | Score | Notes |
   |---|-----------|-------|-------|
   | 1 | Category Fit | {0-2} | {one line} |
   | 2 | Description Quality | {0-2} | {one line} |
   | 3 | Progressive Disclosure | {0-2} | {one line} |
   | 4 | Gotchas | {0-2} | {one line} |
   | 5 | Don't State Obvious | {0-2} | {one line} |
   | 6 | Flexibility | {0-2} | {one line} |
   | 7 | Setup & Config | {0-2} | {one line} |
   | 8 | Memory & State | {0-2} | {one line} |
   | 9 | Scripts & Code | {0-2} | {one line} |
   | 10 | Allowed Tools | {0-2} | {one line} |
   | 11 | Argument Handling | {0-2} | {one line} |
   | 12 | Output & Artifacts | {0-2} | {one line} |

   ### Top 3 Fixes (highest impact)
   1. {fix}
   2. {fix}
   3. {fix}
   ```

7. **Batch mode** — if `$ARGUMENTS` is "all" or "*":
   - Find all skills via Glob
   - Run audit on each
   - Output summary table sorted by score (worst first)
   - Identify common weaknesses across the set

## Gotchas

1. **N/A dimensions still score 2** — if a skill genuinely doesn't need config/state/scripts, don't penalize it. Score N/A as 2. Only score 0-1 when the skill SHOULD have it but doesn't.
2. **Gotchas ≠ Common Issues** — "Common Issues" is troubleshooting (reactive). Gotchas are preventive — things Claude would get WRONG by default. A skill with only "Common Issues" scores 1, not 2.
3. **Line count is a smell, not a rule** — 500-line SKILL.md with no references is suspicious. But 400 lines of dense, non-obvious content is fine. Check if content COULD be extracted, not just if it's long.
4. **Description under 300 chars** — long descriptions dilute the trigger signal. Claude scans ALL skill descriptions at session start. Shorter = sharper matching.

## Common Issues

### Can't find skill
**Cause:** Skill not in expected path or using different directory structure.
**Fix:** Use full path to SKILL.md as argument. Or run with "all" to discover available skills.

### Score seems unfair
**Cause:** Some dimensions don't apply to the skill type.
**Fix:** N/A dimensions score 2. Re-read the rubric — it accounts for genuinely stateless or config-free skills.
