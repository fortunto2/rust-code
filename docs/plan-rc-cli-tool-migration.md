# Plan: rc-cli SgrAction → Tool trait migration

## Goal
rc-cli = reference implementation of sgr-agent architecture.
Replace 27-variant SgrAction enum with Tool impls + ToolRegistry.

## Current state
- backend.rs: SgrAction enum (27 variants), tool_defs(), tool_call_to_sgr_action()
- agent.rs: execute_action() — 1400 lines of match arms with middleware
- Shared state: read_cache, cwd, edit_failures, step_count, swarm, delegates, mcp

## Target architecture
```
rc-cli/src/tools/    ← each tool is a struct impl Tool
  read_file.rs       ← uses sgr-agent-tools::ReadTool + read cache middleware
  write_file.rs      ← uses sgr-agent-tools::WriteTool
  apply_patch.rs     ← uses sgr-agent-tools::ApplyPatchTool
  bash.rs            ← uses sgr-agent-tools::ShellTool + cwd tracking
  search_code.rs     ← uses sgr-agent-tools::SearchTool
  git.rs             ← git_status, git_diff, git_add, git_commit
  editor.rs          ← open_editor
  mcp.rs             ← mcp_call
  memory.rs          ← memory tool
  project_map.rs     ← treesitter project map
  dependencies.rs    ← treesitter deps
  task.rs            ← task management
  delegate.rs        ← delegate_task, delegate_status, delegate_result
  swarm.rs           ← spawn_agent, wait_agents, agent_status, cancel_agent
  api.rs             ← OpenAPI call
  ask_user.rs        ← ask_user (waiting tool)
  finish.rs          ← finish (done tool)

agent.rs:
  - Create ToolRegistry in new()
  - SgrAgent::execute() delegates to registry
  - Remove execute_action() match
  - Shared state in AgentContext typed store

backend.rs:
  - Remove SgrAction enum
  - Remove tool_call_to_sgr_action()
  - tool_defs() generated from ToolRegistry
```

## Shared state → AgentContext typed store
```rust
#[derive(Clone)]
struct RcCliState {
    read_cache: Arc<Mutex<HashMap<String, (String, usize)>>>,
    cwd: Arc<Mutex<PathBuf>>,
    edit_failures: Arc<Mutex<HashMap<String, usize>>>,
    step_count: usize,
}
ctx.insert(RcCliState { ... });
```

## Phase 1: Add deps, create RcCliState
## Phase 2: Convert file tools (7) to Tool impls using sgr-agent-tools
## Phase 3: Convert remaining tools (20) to Tool impls
## Phase 4: Replace SgrAction with ToolRegistry, remove enum
## Phase 5: Update TUI integration
