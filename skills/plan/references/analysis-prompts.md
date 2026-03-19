# Analysis Task Templates

Templates for Phase 2. Pick category matching your goal, customize per project.
Orchestrator fills `{SITUATION}`, `{BUILD_COMMANDS}`, `{FOCUS_AREAS}` from recon.

---

## Project Improvement

### Code Quality

```markdown
## Context
{SITUATION}

## Analyze
1. **Lint & warnings** — run linter ({BUILD_COMMANDS}). All warnings by severity.
2. **Dead code** — unused functions, imports, variables. Search, not just linter.
3. **Error handling** — swallowed errors, unwrap/panic in non-test code, missing context.
4. **Test coverage** — which modules tested? Meaningful tests or smoke?
5. **Code smells** — functions > 100 lines, god objects, duplication, deep nesting.

### Focus areas
{FOCUS_AREAS}

## Results
Bullet list grouped by category, each with file:line and severity (P0/P1/P2).
```

### Architecture

```markdown
## Context
{SITUATION}

## Analyze
1. **Module boundaries** — cohesive? Clear responsibilities? God modules?
2. **Dependency direction** — inward (clean arch)? Or core depends on infra?
3. **API surface** — minimal? Or too many exposed internals?
4. **Coupling** — changing A forces changes in B, C, D?
5. **Patterns** — consistent across codebase? Or each module different?
6. **Abstractions** — missing (repeated patterns)? Over-abstraction?

### Focus areas
{FOCUS_AREAS}

## Results
Bullet list grouped by category, with module/file references.
```

### Performance

```markdown
## Context
{SITUATION}

## Analyze
1. **Hot paths** — most called functions, efficiency.
2. **Allocations** — unnecessary cloning, string allocs in loops, Vec without capacity.
3. **I/O** — sequential that could be concurrent? Unbuffered? Missing pooling?
4. **Concurrency** — locks too long? Contention? Missing parallelism?
5. **Startup** — lazy vs eager init. Blocking I/O at startup.
6. **Data structures** — HashMap vs BTreeMap vs Vec? Iterators vs collecting?

### Focus areas
{FOCUS_AREAS}

## Results
Bullet list with file:line and estimated impact (high/medium/low).
```

---

## Feature Planning

### Existing Patterns

```markdown
## Context
{SITUATION}

## Analyze
1. **Prior art** — 2-3 most similar existing features. How structured?
2. **Conventions** — naming, file layout, module boundaries for this type.
3. **Shared code** — utilities/helpers the new feature should reuse?
4. **Test patterns** — how similar features tested? Unit? Integration? E2E?

### The feature
{FOCUS_AREAS}

## Results
List patterns, files to reuse, conventions to follow.
```

### Integration Points

```markdown
## Context
{SITUATION}

## Analyze
1. **Entry points** — routes, handlers, commands that call this.
2. **Data flow** — consumed? Produced? Schema?
3. **Dependencies** — external services, databases, APIs.
4. **Side effects** — events, notifications, cache invalidation?

### The feature
{FOCUS_AREAS}

## Results
Integration points with file:line, data schemas, dependency graph.
```

### Edge Cases and Risks

```markdown
## Context
{SITUATION}

## Analyze
1. **Edge cases** — boundaries, empty inputs, concurrency, large data.
2. **Backwards compatibility** — breaks existing behavior? API changes?
3. **Security** — auth, injection, secrets, CORS, rate limiting.
4. **Failure modes** — dependency down? Timeout? Rate limited?

### The feature
{FOCUS_AREAS}

## Results
Risks with likelihood (high/medium/low) and mitigation approach.
```

---

## Bug Investigation

### Root Cause

```markdown
## Context
{SITUATION}

## Investigate
1. **Reproduce** — steps to trigger. Reproduce via test/CLI?
2. **Trace** — code path from trigger to symptom. Where diverges?
3. **Recent changes** — git log for affected files. What changed?
4. **Root cause** — actual bug, not symptoms. Why?

### Bug description
{FOCUS_AREAS}

## Results
Root cause with file:line, reproduction steps, confidence level.
```

### Blast Radius

```markdown
## Context
{SITUATION}

## Investigate
1. **Callers** — who calls affected function? What breaks if changed?
2. **Data** — corrupted? How much? Fixable?
3. **Related bugs** — symptom of larger issue? Same pattern elsewhere?
4. **Fix impact** — what does fix touch? What could it break?

### Bug description
{FOCUS_AREAS}

## Results
Affected callers, data state, related code locations.
```

---

## Domain-Specific (optional 4th dimension)

- **Security** — web apps: auth, injection, secrets, CORS, CSRF
- **UX/Accessibility** — frontend: a11y, responsive, loading/error states
- **Data integrity** — DB-heavy: migrations, constraints, orphans, N+1
- **API design** — libraries: ergonomics, backwards compat, docs
- **DevOps** — infra: CI/CD gaps, monitoring, deployment reliability
- **Scalability** — growth: bottlenecks at 10x/100x current load
