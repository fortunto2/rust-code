# Skills CLI Commands

## Search (remote, 60K+ skills)
```bash
rust-code skills search <query>
```
Examples: `search react performance`, `search pr review`, `search changelog`

## Catalog (cached popular skills)
```bash
rust-code skills catalog [query]
# --refresh to force update cache
```

## Install
```bash
rust-code skills add <owner/repo/skill-name>
# Example: rust-code skills add anthropics/claude-code/memory
```

## List installed
```bash
rust-code skills list
# --brief for names only
```

## Show skill content
```bash
rust-code skills show <name>
```

## Remove
```bash
rust-code skills remove <name>
```

## Browse online
https://skills.sh/
