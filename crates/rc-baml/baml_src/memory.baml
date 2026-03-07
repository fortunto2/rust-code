// Typed agent memory — structured JSONL instead of free-form markdown.
//
// BAML defines the schema → agent writes typed entries → stored as JSONL.
// Other agents can define their own .baml memory schemas.
// Markdown (MEMORY.md) stays for user-editable free-form notes.

class MemoryTool {
  tool_name "memory" @description("Save or forget a learning in persistent typed memory (MEMORY.jsonl). Survives across sessions. Use for: confirmed patterns, key decisions, user preferences, debugging insights. Do NOT save: temporary task state, speculative conclusions, things already in project CLAUDE.md/AGENTS.md.")
  operation "save" | "forget" @description("save = add new entry, forget = remove entries matching section")
  category "decision" | "pattern" | "preference" | "insight" | "debug" @description("decision = architectural choice with reasoning, pattern = reusable approach, preference = user/project style, insight = debugging finding, debug = temporary debug note")
  section string @description("Topic grouping, e.g. 'Model Findings', 'Build System', 'User Preferences'")
  content string @description("The actual learning, decision, or pattern")
  context string | null @description("Why this was recorded — what triggered it. Null for forget operations.")
  confidence "confirmed" | "tentative" @description("confirmed = verified across multiple sessions, tentative = first observation")
}
