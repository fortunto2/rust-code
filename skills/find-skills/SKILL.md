---
name: find-skills
description: Helps users discover and install agent skills when they ask questions like "how do I do X", "find a skill for X", "is there a skill that can...", or express interest in extending capabilities. This skill should be used when the user is looking for functionality that might exist as an installable skill.
---

# Find Skills

This skill helps you discover and install skills from the open agent skills ecosystem (60K+ skills on skills.sh).

## When to Use This Skill

Use this skill when the user:

- Asks "how do I do X" where X might be a common task with an existing skill
- Says "find a skill for X" or "is there a skill for X"
- Asks "can you do X" where X is a specialized capability
- Expresses interest in extending agent capabilities
- Wants to search for tools, templates, or workflows
- Mentions they wish they had help with a specific domain (design, testing, deployment, etc.)

## Skills CLI Commands

**Search the full database (60K+ skills):**

```bash
rust-code skills search <query>
```

**Browse top skills by popularity (cached):**

```bash
rust-code skills catalog [query]
```

**Install a skill:**

```bash
rust-code skills add <owner/repo/skill-name>
```

**List installed skills:**

```bash
rust-code skills list
```

**Remove a skill:**

```bash
rust-code skills remove <name>
```

**Browse skills at:** https://skills.sh/

## How to Help Users Find Skills

### Step 1: Understand What They Need

When a user asks for help with something, identify:

1. The domain (e.g., React, testing, design, deployment)
2. The specific task (e.g., writing tests, creating animations, reviewing PRs)
3. Whether this is a common enough task that a skill likely exists

### Step 2: Search for Skills

Run the search command with a relevant query:

```bash
rust-code skills search <query>
```

For example:

- User asks "how do I make my React app faster?" -> `rust-code skills search react performance`
- User asks "can you help me with PR reviews?" -> `rust-code skills search pr review`
- User asks "I need to create a changelog" -> `rust-code skills search changelog`

### Step 3: Present Options to the User

When you find relevant skills, present them to the user with:

1. The skill name and what it does
2. The install command
3. Install count (popularity indicator)

### Step 4: Install

If the user wants to proceed, install the skill:

```bash
rust-code skills add <owner/repo/skill-name>
```

## Common Skill Categories

| Category        | Example Queries                          |
| --------------- | ---------------------------------------- |
| Web Development | react, nextjs, typescript, css, tailwind |
| Testing         | testing, jest, playwright, e2e           |
| DevOps          | deploy, docker, kubernetes, ci-cd        |
| Documentation   | docs, readme, changelog, api-docs        |
| Code Quality    | review, lint, refactor, best-practices   |
| Design          | ui, ux, design-system, accessibility     |
| Productivity    | workflow, automation, git                |

## When No Skills Are Found

If no relevant skills exist:

1. Acknowledge that no existing skill was found
2. Offer to help with the task directly using your general capabilities
3. Suggest browsing https://skills.sh/ for more options
