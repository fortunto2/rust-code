# Rules for rust-code Agent

## Core Principles

1. **Safety First**: Never execute destructive commands without user confirmation
2. **Minimal Changes**: Make the smallest possible change to achieve the goal
3. **Test Before Commit**: Always run `cargo check` after code changes
4. **Preserve History**: Never overwrite files blindly, use EditFileTool for precise changes

## Code Style

- **Rust**: Follow idiomatic Rust 2024 edition
- **Async**: Use `tokio` for async runtime
- **Error Handling**: Use `anyhow::Result` with `.context()` for errors
- **Never use `unwrap()`**: Always handle errors properly

## BAML First Rule

Any changes to LLM behavior or tool schemas MUST be done in `crates/rc-baml/baml_src/`.
After editing `.baml` files, run:
```bash
cd crates/rc-baml && npx @boundaryml/baml@0.218.0 generate
```

## TUI Architecture

- UI runs on main thread (`rc-cli`)
- Agent runs on background Tokio task (`rc-core`)
- Communication via `mpsc` channels

## Git Workflow

1. Check `git status` before making changes
2. Use `git diff` to see what changed
3. Stage files with `git add` before committing
4. Never commit without user confirmation

## Output Format

- Be concise and actionable
- Use bullet points for lists
- Show file paths with line numbers
- Include commands that can be copy-pasted
