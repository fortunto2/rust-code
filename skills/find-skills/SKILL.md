---
name: find-skills
description: Discover and install agent skills from skills.sh (60K+ skills). Use when "find a skill", "search skills", "is there a skill for", "install skill", "skill catalog". Do NOT use for auditing existing skills (use /skill-audit) or browsing local skills (use `skills list`).
allowed-tools: bash, read_file, finish
argument-hint: "<search query or skill name>"
---

# Find Skills

Discover and install skills from the skills.sh ecosystem.

CLI reference: `references/cli-commands.md`

## Workflow

1. Understand what the user needs (domain + specific task)
2. Search: `rust-code skills search <query>` or browse: `rust-code skills catalog [query]`
3. Present top results with install counts
4. Install: `rust-code skills add <owner/repo/skill-name>`

## Gotchas

1. **`search` hits skills.sh API, `catalog` is cached** — search is slower but comprehensive (60K+). Catalog is fast but limited to popular skills. Try catalog first, fall back to search.
2. **Not all skills work with rust-code** — skills.sh skills are designed for Claude Code. Most work, but some depend on Claude Code-specific features (MCP, hooks). Check SKILL.md after install.
3. **Skill names can collide** — if user has a local skill with the same name, the local one takes priority. Use `skills list` to check for conflicts.
4. **Install is a git clone** — requires internet. The skill repo is cloned into the skills directory. No npm/pip involved.
