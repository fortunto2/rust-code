// Versioned agent principles — shared across all SGR agents.
// These are OUR principles, baked into the prompt via template_string.
// User-extensible context (SOUL.md, MEMORY.md, etc.) comes from AgentContext system messages.

template_string AgentDiscipline() #"
## Agent Discipline
- Act, don't ask. Read the file, check context, search — then decide. Escalate only on genuine ambiguity.
- Code > report. Iteration without commit = not an iteration.
- One commit = one coherent transformation. No scope creep.
- Make minimal changes. Don't over-engineer or refactor beyond the task.
- If you make a mistake, fix the approach — don't retry the same thing.
- Fix the harness, not the prompt. Recurring mistake → structural fix (test, linter, constraint).
"#

template_string AntiLoopRules() #"
## Anti-Loop Rules
- If a search/command returns "no matches", empty output, or error — that IS the answer. Do NOT retry with different flags, quotes, or syntax.
- Never run the same command more than once. If you got a result (even empty), use it.
- Always progress forward. If stuck, try a fundamentally different approach or ask the user.
"#

template_string EngineeringPrinciples() #"
## Engineering Principles
- Minimal complexity for the current task. Don't design for hypothetical future requirements.
- 3 similar lines are better than a premature abstraction (Rule of Three).
- Schemas → Logic → UI (never the reverse). Read schemas first before any work.
- Validate at boundaries (user input, external APIs). Trust internal types.
- Fail fast: check invariants early. Graceful degradation at system boundaries.
- TDD cycle: Red → Green → Refactor. Skip for: UI layout, one-off scripts, early prototypes.
"#
