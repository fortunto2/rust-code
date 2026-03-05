# Project Conventions

## Architecture

This is a Rust workspace with 4 crates:
- `rc-baml`: BAML schemas and LLM client
- `rc-core`: Agent logic and tool execution
- `rc-tools`: Tool implementations (fs, bash, search, git)
- `rc-cli`: TUI interface with ratatui

## File Organization

```
crates/
├── rc-baml/baml_src/     # All BAML schemas
├── rc-core/src/agent.rs  # Agent loop
├── rc-tools/src/         # Tool implementations
└── rc-cli/src/           # TUI code
```

## Naming Conventions

- **Files**: `snake_case.rs`
- **Structs/Enums**: `PascalCase`
- **Functions/Variables**: `snake_case`
- **Constants**: `SCREAMING_SNAKE_CASE`

## Tools Available

### File Operations
- `ReadFileTool(path, offset?, limit?)` — Read file with pagination
- `WriteFileTool(path, content)` — Write new file
- `EditFileTool(path, old_string, new_string)` — Replace text

### Search
- `SearchCodeTool(query)` — Search with ripgrep
- `BashCommandTool(command)` — Execute shell commands

### Git
- `GitStatusTool()` — Get git status
- `GitDiffTool(path?, cached?)` — Show diff
- `GitAddTool(paths)` — Stage files
- `GitCommitTool(message)` — Create commit

### Navigation
- `OpenEditorTool(path, line?)` — Open in $EDITOR

## Response Style

When analyzing code:
1. Quote relevant code snippets
2. Explain the "why" not just "what"
3. Suggest specific fixes with line numbers
4. Offer alternatives when applicable
