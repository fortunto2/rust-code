---
name: plan
description: Research codebase and create spec + phased implementation plan with file-level tasks
triggers: [plan, spec, battle_plan]
priority: 10
keywords: [plan, spec, feature, bug, refactor, implement, design, breakdown]
---

WORKFLOW:
  1. READ CLAUDE.md + README.md — architecture, stack, constraints
  2. TREE root level=3 — understand project structure
  3. SEARCH for keywords from task description — find affected files
  4. READ existing plans — list("docs/plan") to check overlap
  5. GENERATE track ID: {shortname}_{YYYYMMDD} (kebab-case)
  6. WRITE docs/plan/{trackId}/spec.md:
     ```
     # Specification: {Title}
     **Track ID:** {trackId}
     **Type:** Feature|Bug|Refactor|Chore
     **Created:** {date}
     ## Summary — 1-2 paragraphs from research
     ## Acceptance Criteria — 3-8 concrete checkboxes
     ## Dependencies
     ## Out of Scope
     ## Technical Notes — architecture decisions, reusable code found
     ```
  7. WRITE docs/plan/{trackId}/plan.md:
     ```
     # Implementation Plan: {Title}
     **Spec:** [spec.md](./spec.md)
     ## Phase 1: {Name}
     ### Tasks
     - [ ] Task 1.1: {description with file paths}
     ### Verification
     - [ ] {check}
     ## Phase N: Docs & Cleanup
     - [ ] Update CLAUDE.md
     - [ ] Remove dead code
     ## Context Handoff
     ### Key Files — from research
     ### Decisions Made — why X over Y
     ### Risks
     ```
  8. UPDATE_PLAN with phases as steps (pending/in_progress/completed)
  9. FINISH with: "Track {trackId}: {N} phases, {N} tasks"

RULES:
  - Every task mentions specific FILE PATHS (from research, not guessed)
  - 5-15 tasks total across 2-4 phases
  - Last phase always "Docs & Cleanup"
  - Every acceptance criterion maps to at least one task
  - Tasks are atomic — one commit each

WRONG: Plan without reading code first (guessing file paths)
CORRECT: research → spec → plan → update_plan → finish

EXAMPLE — Plan a new feature:
  Instruction: "plan adding WebSocket support"
  1. read("CLAUDE.md") → understand architecture
  2. tree("/", 3) → find relevant modules
  3. search("/", "websocket|ws|socket") → check existing code
  4. write("docs/plan/websocket-support_20260412/spec.md", ...)
  5. write("docs/plan/websocket-support_20260412/plan.md", ...)
  6. update_plan(plan: [{step: "Phase 1: Protocol", status: "pending"}, ...])
  7. finish("Track websocket-support_20260412: 3 phases, 8 tasks")
