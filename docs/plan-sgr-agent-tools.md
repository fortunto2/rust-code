# Plan: sgr-agent-tools crate

## Goal
Extract universal tools from agent-bit into reusable crate. Any agent project gets battle-tested tools out of the box.

## Architecture (inspired by Codex RS + Claude Code)

### FileBackend trait
```rust
pub trait FileBackend: Send + Sync {
    async fn read(&self, path: &str, offset: usize, limit: usize) -> Result<String>;
    async fn write(&self, path: &str, content: &str) -> Result<()>;
    async fn delete(&self, path: &str) -> Result<()>;
    async fn search(&self, root: &str, pattern: &str, limit: i32) -> Result<String>;
    async fn list(&self, path: &str) -> Result<String>;
    async fn tree(&self, root: &str, level: i32) -> Result<String>;
    async fn context(&self) -> Result<String>; // date/time/env
}
```

### Tools (10 active + N deferred)

**CORE (always active):**
| Tool | Codex equiv | Claude Code equiv | Notes |
|------|------------|-------------------|-------|
| `ReadTool<B>` | read_file | FileReadTool | trust metadata, auto line numbers |
| `WriteTool<B>` | apply_patch | FileWriteTool | JSON repair (llm_json), hooks |
| `DeleteTool<B>` | (shell rm) | (Bash rm) | policy check |
| `SearchTool<B>` | grep_files | GrepTool | auto-expand ≤10 files, CRM annotations |
| `ListTool<B>` | list_dir | GlobTool | |
| `TreeTool<B>` | (shell tree) | — | workspace overview |
| `EvalTool<B>` | js_repl | — | Boa JS engine, file glob, workspace_date |
| `ReadAllTool<B>` | — | — | batch read directory |

**PAC1-specific (NOT in crate):**
- AnswerTool — submit answer to harness
- ContextTool — get workspace date/time

### Codex RS patterns to adopt

1. **Indentation-aware read** (`read_file.rs:836 lines`)
   - `anchor_line` + `max_levels` + `include_siblings`
   - Smart block extraction, not just offset/limit
   - Reduces context waste on large files

2. **Apply_patch (diff-based edit)** (`apply_patch.rs:545 lines`)
   - Freeform diff input (LLM-friendly)
   - Lark grammar parser
   - Saves tokens vs full file write
   - Fallback to shell (`git apply`)

3. **Parallel execution** (`parallel.rs:148 lines`)
   - `RwLock<()>` coordination
   - Per-tool `supports_parallel` flag
   - Read-only tools = concurrent, mutating = sequential

4. **Sandbox traits** (`sandboxing.rs:529 lines`)
   ```rust
   trait Sandboxable {
       fn sandbox_preference(&self) -> SandboxablePreference;
       fn escalate_on_failure(&self) -> bool;
   }
   trait Approvable<Rq> {
       fn approval_keys(&self, req: &Rq) -> Vec<Self::ApprovalKey>;
   }
   ```

5. **Tool payload flexibility** — JSON + freeform + custom modes

6. **JS via subprocess** (not embedded engine)
   - Codex uses Node.js subprocess with kernel.js (1369 lines)
   - JSON line protocol bidirectional
   - Nested tool calls from JS back to host
   - We use Boa (embedded) — simpler but less powerful
   - Future: option for both (Boa default, Node.js if available)

7. **Deferred tools** (sgr-agent registry)
   - Already implemented in sgr-agent
   - Model sees names only, loads schema on demand
   - Reduces prompt token overhead

8. **Hook system** (hooks.rs)
   - Post-tool hooks parsed from AGENTS.MD
   - Delivered via tool output (model follows tool output > system prompt)
   - Generic: HookRegistry.check(tool, path) → messages

### Implementation phases

**Phase 1: Extract core tools**
- Create `crates/sgr-agent-tools/` in rust-code workspace
- Move ReadTool, WriteTool, SearchTool, etc. with FileBackend generic
- agent-bit implements `FileBackend for PcmClient`
- rc-cli implements `FileBackend for LocalFs`

**Phase 2: Add Codex patterns**
- Indentation-aware read mode
- Apply_patch tool (diff-based edit)
- Parallel execution support

**Phase 3: JS integration options**
- Keep Boa as default (zero dependency, sandboxed)
- Add optional Node.js subprocess mode (more powerful)
- Feature flag: `features = ["node-repl"]`

**Phase 4: Sandbox + approval**
- Sandboxable trait
- Approval caching
- Policy engine integration

## Backends

| Backend | Project | Implementation |
|---------|---------|---------------|
| PcmClient | agent-bit (PAC1) | BitGN Connect-RPC |
| LocalFs | rc-cli | std::fs |
| MiniRuntime | agent-bit (mini) | BitGN mini protocol |
| MockFs | tests | In-memory HashMap |

## Dependencies
- `sgr-agent` (Tool trait, registry, deferred tools)
- `boa_engine` (JS eval)
- `llm_json` (JSON repair)
- `regex` (search patterns)
- `chrono` (date operations)

## References
- Codex RS: `/Users/rustam/startups/shared/codex/codex-rs/core/src/tools/`
- Claude Code (HitCC): `/Users/rustam/startups/shared/HitCC/docs/02-execution/`
- PAC1 agent: `/Users/rustam/startups/active/agent-bit/src/tools.rs`
- Skill guide: `/Users/rustam/startups/solopreneur/solo-factory/templates/principles/agent-tool-design/`
