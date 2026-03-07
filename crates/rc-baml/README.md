# rc-baml — BAML Prompts for rust-code

BAML (Basically, A Made-Up Language) — DSL for type-safe LLM prompts with structured output.

## Files

| File | What |
|------|------|
| `baml_src/main.baml` | Generator config (Rust, output dir, version) |
| `baml_src/clients.baml` | LLM providers, fallback/round-robin, retry policy |
| `baml_src/agent.baml` | Tool schemas (classes), NextStep union, agent prompt |

## How BAML Works

BAML uses **prompt-based structured output** — it injects the output schema into the prompt via `{{ ctx.output_format }}` and parses the model's text response. It does NOT use native JSON mode or function calling.

This means:
- The model sees a text description of the expected JSON structure
- Quality of tool selection depends on how well the model understands the schema
- Discriminator fields (`tool_name`) are critical for large unions

## Writing New Tool Classes

### 1. Always add a `tool_name` discriminator

```baml
class MyNewTool {
  tool_name "my_new_tool" @description("Use this tool to ... (clear when/why to use it)")
  arg1 string @description("What this argument does")
  arg2 int? @description("Optional argument")
}
```

**Why:** Without `tool_name`, the model disambiguates by field names alone. If two tools share field names (e.g. both have `path`), the model picks the wrong one. The literal string discriminator forces the model to explicitly choose.

**Rules:**
- `tool_name` must be a **string literal** (in quotes), not a free string
- Put a clear **"Use this tool to/when..."** in the `@description` — this is what the model reads to decide
- Describe what the tool does NOT do if it's easily confused with another (see `GitDiffTool` vs `ReadFileTool`)

### 2. Add to the union in NextStep

```baml
class NextStep {
  actions (... | MyNewTool | ...)[] @stream.not_null @description("...")
}
```

The union name will change (e.g. `Union14...` → `Union15...`). After regenerating, update all references in `rc-cli/src/` with sed.

### 3. Add the match arm in `agent.rs`

```rust
MyNewTool(cmd) => {
    // implement tool logic
    Ok(AgentEvent::Message(format!("Result: {}", output)))
}
```

## Writing Prompts

### Template syntax

```baml
function MyFunction(input: MyInput) -> MyOutput {
  client AgentFallback
  prompt #"
    {{ _.role("system") }}
    System instructions here.

    {{ _.role("user") }}
    {{ input.field }}

    {{ ctx.output_format }}
  "#
}
```

### Key rules

1. **Always include `{{ ctx.output_format }}`** — this injects the JSON schema. Without it, the model doesn't know what to output.

2. **Use `_.role()` for message boundaries** — `{{ _.role("system") }}`, `{{ _.role("user") }}`, `{{ _.role("assistant") }}`.

3. **Use Jinja2 for loops/conditions:**
   ```baml
   {% for msg in history %}
   {{ _.role(msg.role) }}
   {{ msg.content }}
   {% endfor %}
   ```

4. **Don't repeat the schema manually** — `ctx.output_format` handles it. Adding manual field descriptions creates conflicts.

5. **`@description` on fields IS part of the prompt** — BAML injects these into `ctx.output_format`. Write them as instructions to the model, not as code docs.

6. **`@stream.not_null`** — add to fields that must be complete before streaming yields. Use on the `action` field so partial tool objects don't get dispatched.

## Client Configuration

### Providers

```baml
client<llm> MyClient {
  provider google-ai          // google-ai, openai, anthropic, etc.
  retry_policy Backoff        // reference to retry_policy block
  options {
    model "gemini-3.1-pro-preview"
    api_key env.GEMINI_API_KEY
  }
}
```

### Fallback (auto-failover)

```baml
client<llm> MyFallback {
  provider fallback
  options {
    strategy [ClientA, ClientB, ClientC]  // tries in order
  }
}
```

### Round-robin (load balancing)

```baml
client<llm> MyBalanced {
  provider round-robin
  options {
    strategy [ClientA, ClientB]  // alternates between
  }
}
```

### Retry policy

```baml
retry_policy Backoff {
  max_retries 3
  strategy {
    type exponential_backoff
    delay_ms 500
    multiplier 2
    max_delay_ms 10000
  }
}
```

## Build Workflow

```bash
# 1. Edit .baml files
vim crates/rc-baml/baml_src/agent.baml

# 2. Regenerate Rust client
~/.cargo/bin/baml-cli generate --from crates/rc-baml/baml_src

# 3. Sync to rc-cli (it has its own copy, not a symlink)
rm -rf crates/rc-cli/src/baml_client && cp -r crates/rc-baml/src/baml_client crates/rc-cli/src/baml_client

# 4. If union changed (added/removed tool), update references:
# Old: Union14AskUserTool...
# New: Union15AskUserTool...
# Find with: grep -r "Union1[0-9]" crates/rc-cli/src/

# 5. Build
cargo build

# 6. Test
cargo run -- -p "test prompt here"
```

## Lessons Learned

| Problem | Cause | Fix |
|---------|-------|-----|
| Model picks GitDiffTool instead of ReadFileTool | Both have `path` field, no discriminator | Add `tool_name` literal discriminator |
| Model outputs YAML instead of JSON | Prompt-based output, weaker model | Use stronger model (Pro) or fallback chain |
| Model loops on same wrong tool | Small models can't self-correct | Loop detection (warn at 3, abort at 6) + fallback to stronger model |
| `baml-cli generate` not found | Not installed via npm | Installed via cargo: `~/.cargo/bin/baml-cli` |
| Union type name changes | Adding/removing tools changes the enum name | sed across agent.rs, main.rs, app.rs |
