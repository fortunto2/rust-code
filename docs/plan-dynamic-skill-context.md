# Plan: Dynamic Context Injection in Skills

## Idea
From Claude Code skills pattern: `!<command>` in SKILL.md executes before sending to LLM.
Output replaces placeholder. Agent gets real data without spending tool calls.

## Syntax
```markdown
---
name: inbox-processing
---

## Current workspace
- Tree: `!tree /`
- Inbox: `!read_all 00_inbox`
- Channels: `!search channel docs/channels`

## Workflow
Process each inbox message...
```

## Implementation

### For shell-based agents (rc-cli, Codex-style)
```rust
// In skills.rs load_skill():
fn inject_dynamic_context(body: &str, shell: &ShellBackend) -> String {
    let re = regex::Regex::new(r"`!(.+?)`").unwrap();
    re.replace_all(body, |caps: &regex::Captures| {
        let cmd = &caps[1];
        shell.exec(cmd).unwrap_or_else(|e| format!("(error: {e})"))
    }).to_string()
}
```

### For API-based agents (agent-bit with PCM)
```rust
// Custom executor using FileBackend trait:
async fn inject_dynamic_context<B: FileBackend>(body: &str, backend: &B) -> String {
    let re = regex::Regex::new(r"`!(\w+)\s*(.*?)`").unwrap();
    // Parse: !tree /, !read path, !search pattern path, !list dir
    for cap in re.captures_iter(body) {
        let cmd = &cap[1];
        let args = &cap[2];
        let output = match cmd {
            "tree" => backend.tree(args, 2).await,
            "read" => backend.read(args, false, 0, 0).await,
            "list" => backend.list(args).await,
            "search" => { let parts: Vec<&str> = args.splitn(2, ' ').collect();
                          backend.search(parts.get(1).unwrap_or(&"/"), parts[0], 10).await },
            _ => Ok(format!("(unknown: {cmd})")),
        };
        // Replace in body
    }
}
```

### Feature flag
```toml
[features]
skill-dynamic-context = []  # enables !command execution in SKILL.md

# In settings:
# disableSkillShellExecution: true  — safety override
```

## Benefits for PAC1
- Inbox tasks: inject inbox content in skill instead of pre-grounding
- Finance tasks: inject account list in skill
- Query tasks: inject entity cast names
- Saves 2-5 tool calls per task

## Security
- Only whitelisted commands (tree, read, list, search, context)
- No arbitrary shell exec for API backends
- disableSkillShellExecution config option
- Rate limit: max 5 injections per skill load

## Priority
Medium — after leaderboard stabilizes. Good for v1.0 release.
