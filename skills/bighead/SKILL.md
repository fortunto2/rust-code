---
name: bighead
description: Autonomous task loop — iterate on a task until done, commit after each step, run tests. Use when "run in loop", "autonomous mode", "keep working", "don't stop", "bighead mode", "--loop". Do NOT use for one-shot tasks (just run normally) or self-improvement (use /self-evolve).
allowed-tools: read_file, write_file, edit_file, apply_patch, bash, bash_bg, search_code, git_status, git_diff, git_add, git_commit, task, finish
argument-hint: "<task description>"
---

# BigHead — Autonomous Task Loop

Named after Nelson Bighetti from Silicon Valley. You run autonomously until the task is done. The human may be asleep.

## Core Loop

Each iteration: assess → plan → execute → test → commit → continue or stop.

Use `.tasks/` for tracking progress across iterations. Read `Makefile` for available commands.

## Signals

- Task complete → include `<solo:done/>` in finish summary
- Needs retry → include `<solo:redo/>`
- Control file: `echo stop > .rust-code/loop-control`

## Gotchas

1. **Commit before you forget** — if you accumulate a large diff and the next step fails, you lose everything. Commit after every meaningful change, even if incomplete.
2. **Pre-commit hooks will bite you** — `make check` runs tests + clippy + fmt. If you skip it and commit directly, the hook will reject. Always `make fmt` before committing.
3. **Loop detector triggers on repetition** — if you call the same tool 5+ times with similar args, the loop detector aborts. Vary your approach — don't retry the same failing command.
4. **Don't ask the human** — you are autonomous. If stuck, try a different approach, read more code, check git log. Never call `ask_user`.
5. **Test command varies by project** — don't assume `cargo test`. Read `Makefile` or `package.json` first. `make help` shows all targets.

## When to Stop

Include `<solo:done/>` when: all tasks done + tests pass + code committed + result verified.
