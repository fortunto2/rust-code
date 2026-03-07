# baml-agent

Shared Rust crate for building BAML-powered SGR (Schema-Guided Reasoning) agents.

Reusable across multiple agent projects — just implement `SgrAgent` trait and wire your BAML-generated types.

## What is SGR?

Schema-Guided Reasoning — the LLM generates structured JSON (not function calls) guided by a schema that BAML injects into the prompt via `{{ ctx.output_format }}`. The model fills in a discriminator field (`task`) to pick which tool to use, and the agent loop executes it.

```
User request → [SGR Loop] → decide (LLM) → execute (tools) → push result → repeat
                                ↑                                    |
                                └────────────────────────────────────┘
```

## Modules

| Module | What |
|--------|------|
| `config` | `AgentConfig`, `ProviderConfig` — multi-provider LLM config (Vertex AI, Google AI, OpenAI-compatible) |
| `engine` | `BamlRegistry` trait, `AgentEngine` — builds BAML ClientRegistry from config |
| `session` | `Session<M>`, `AgentMessage`, `MessageRole`, `EntryType`, `MessageBody`, `MessageContent`, `ContentBlock`, `SessionMeta`, `list_sessions`, `search_sessions` — JSONL persistence with typed structs, UUID v7 IDs, Claude Code compatible format, history trimming, session browsing. Split into submodules: `traits` (message traits), `format` (serialization/deserialization), `time` (UUID v7 timestamp extraction, UTF-8 safe truncation), `store` (Session struct, persistence), `meta` (SessionMeta, listing, search) |
| `loop_detect` | `LoopDetector`, `LoopStatus`, `normalize_signature` — 3-tier loop detection (exact, semantic, output) |
| `agent_loop` | `SgrAgent`, `SgrAgentStream`, `run_loop`, `run_loop_stream` — the core agent loop |
| `prompt` | `BASE_SYSTEM_PROMPT`, `build_system_prompt()` — STAR system prompt template |
| `helpers` | `norm`, `action_result_from`, `truncate_json_array`, `AgentContext` — reusable patterns + context loading |
| `logging` | `init_logging()` — daily file logging via tracing-appender (feature `logging`) |
| `telemetry` | `init_telemetry()`, `TelemetryGuard` — OTEL-aware JSONL with trace context (feature `telemetry`) |

## Logging & Telemetry

Two optional features for structured logging:

### Feature `logging` — simple file logs

```toml
baml-agent = { path = "../baml-agent", features = ["logging"] }
```

```rust
use baml_agent::init_logging;

// Appends to .agent/agent-2026-03-07.log (daily rotation)
let _guard = init_logging(".agent", "agent");
```

Plain text output via `tracing-appender`. Good for simple CLI agents.

### Feature `telemetry` — OTEL-aware structured JSONL

```toml
baml-agent = { path = "../baml-agent", features = ["telemetry"] }
```

```rust
use baml_agent::{init_telemetry, TelemetryGuard};

// Output: .agent/coach-2026-03-07.jsonl
let _guard: TelemetryGuard = init_telemetry(".agent", "coach");

// All tracing events go to the JSONL file with span context
let span = tracing::info_span!("coaching_turn", turn = 3);
let _enter = span.enter();
tracing::info!(model = "gemini-flash", latency_ms = 420, "LLM response");
```

Each JSONL line includes:
- `timestamp`, `level`, `target`, `message`, `fields`
- `span` / `spans` — current span context (name + fields)
- OTEL trace_id / span_id propagation (via `tracing-opentelemetry`)

**What gets captured:**
- All `tracing::info!()`, `tracing::warn!()`, `tracing::error!()` events
- `#[tracing::instrument]` on functions → automatic span context
- BAML runtime `log` crate output (via `tracing-log` bridge)
- BAML's direct stderr output is suppressed (`BAML_LOG=off`)
- Default filter: `info`+, with `hyper`/`h2`/`reqwest` suppressed

**Example JSONL output:**
```json
{
  "timestamp": "2026-03-07T20:34:24Z",
  "level": "INFO",
  "fields": {
    "message": "coach_decision",
    "agent": "coach",
    "stage": "Opening",
    "sentiment": "Skeptical",
    "coaching_type": "ObjectionHandler"
  },
  "target": "souffleur_sim::runner",
  "span": { "scenario": "university", "disc": "Dominant", "turn": 1, "name": "sim_turn" },
  "spans": [{ "scenario": "university", "disc": "Dominant", "turn": 1, "name": "sim_turn" }]
}
```

**Async spans** — use `tracing::Instrument` trait (not `.entered()` which is `!Send`):

```rust
use tracing::Instrument;

let span = tracing::info_span!("my_turn", turn = n);
async {
    tracing::info!("inside span");  // gets span context
    some_async_call().await;
}.instrument(span).await;
```

**Custom filter** via `RUST_LOG` env var:
```bash
RUST_LOG=debug cargo run  # all debug+ events
RUST_LOG=souffleur_sim=debug,info cargo run  # debug for sim, info for rest
```

### TUI telemetry (baml-agent-tui)

For TUI apps, use `init_tui_telemetry()` which also redirects stderr to a file (prevents BAML's raw stderr output from corrupting the ratatui alternate screen):

```rust
use baml_agent_tui::init_tui_telemetry;

let _guard = init_tui_telemetry(".agent", "tui");
// stderr → .agent/stderr.log
// telemetry → .agent/tui-YYYY-MM-DD.jsonl
```

## Quick Start

### 1. Add dependency

In your project (or use a symlink for local dev):

```toml
[dependencies]
baml-agent = { path = "../baml-agent" }
```

### 2. Implement the traits

```rust
use baml_agent::{
    AgentConfig, AgentEngine, BamlRegistry,
    Session, AgentMessage, MessageRole,
    SgrAgent, StepDecision, ActionResult, LoopConfig, LoopEvent, run_loop,
    action_result_from, action_result_done,  // helpers
};
use std::collections::HashMap;

// --- Wrap your BAML-generated ClientRegistry ---

struct MyRegistry(baml_client::ClientRegistry);

impl BamlRegistry for MyRegistry {
    fn new() -> Self { Self(baml_client::ClientRegistry::new()) }
    fn add_llm_client(&mut self, name: &str, provider_type: &str, options: HashMap<String, serde_json::Value>) {
        self.0.add_llm_client(name, provider_type, options);
    }
    fn set_primary_client(&mut self, name: &str) { self.0.set_primary_client(name); }
}

// --- Wrap your BAML-generated message types ---

#[derive(Clone, Debug, PartialEq)]
struct MyRole(baml_client::types::Role);

impl MessageRole for MyRole {
    fn system() -> Self { Self(Role::System) }
    fn user() -> Self { Self(Role::User) }
    fn assistant() -> Self { Self(Role::Assistant) }
    fn tool() -> Self { Self(Role::Tool) }
    fn as_str(&self) -> &str { /* match self.0 */ }
    fn parse_role(s: &str) -> Option<Self> { /* match s */ }
}

#[derive(Clone)]
struct MyMsg { role: MyRole, content: String }

impl AgentMessage for MyMsg {
    type Role = MyRole;
    fn new(role: MyRole, content: String) -> Self { Self { role, content } }
    fn role(&self) -> &MyRole { &self.role }
    fn content(&self) -> &str { &self.content }
}

// --- Implement SgrAgent ---

struct MyAgent {
    registry: baml_client::ClientRegistry,
}

impl SgrAgent for MyAgent {
    type Action = MyActionUnion;  // BAML-generated union type
    type Msg = MyMsg;
    type Error = String;

    async fn decide(&self, messages: &[MyMsg]) -> Result<StepDecision<MyActionUnion>, String> {
        let baml_msgs = messages.iter().map(|m| m.to_baml()).collect::<Vec<_>>();
        let decision = B.MyDecideFunction
            .with_client_registry(&self.registry)
            .call(&baml_msgs)
            .await
            .map_err(|e| e.to_string())?;

        Ok(StepDecision {
            situation: decision.current_state,
            task: decision.plan,
            completed: decision.task_completed,
            actions: decision.next_actions,
        })
    }

    async fn execute(&self, action: &MyActionUnion) -> Result<ActionResult, String> {
        match action {
            MyActionUnion::SearchTask(t) => {
                Ok(action_result_from(do_search(&t.query)))
            }
            MyActionUnion::FinishTask(t) => {
                Ok(action_result_done(&t.summary))
            }
        }
    }

    fn action_signature(action: &MyActionUnion) -> String {
        // Unique string for loop detection
        match action {
            MyActionUnion::SearchTask(t) => format!("search:{}", t.query),
            MyActionUnion::FinishTask(_) => "finish".into(),
        }
    }
}
```

### 3. Run the loop

```rust
#[tokio::main]
async fn main() {
    // Build registry from config
    let config = AgentConfig::vertex_from_env().unwrap();
    let engine = AgentEngine::new(config);
    let reg: MyRegistry = engine.build_registry().unwrap();

    // Create session
    let mut session = Session::<MyMsg>::new(".sessions", 60).unwrap();
    session.push(MyRole::user(), "Find competitors for my SaaS idea".into());

    // Build agent and run
    let agent = MyAgent { registry: reg.0 };
    let loop_config = LoopConfig { max_steps: 25, loop_abort_threshold: 6 };

    let steps = run_loop(&agent, &mut session, &loop_config, |event| {
        match event {
            LoopEvent::StepStart(n) => println!("\n[Step {}]", n),
            LoopEvent::Decision { situation, task } => {
                println!("Situation: {}", situation);
                for (i, s) in task.iter().enumerate() { println!("  {}. {}", i+1, s); }
            }
            LoopEvent::Completed => println!("Done!"),
            LoopEvent::ActionStart(a) => println!("  > {:?}", a),
            LoopEvent::ActionDone(_) => {}
            LoopEvent::LoopWarning(n) => eprintln!("  ! {} repeats", n),
            LoopEvent::LoopAbort(n) => eprintln!("  ! Aborted after {} repeats", n),
            LoopEvent::Trimmed(n) => eprintln!("  (trimmed {} messages)", n),
            LoopEvent::MaxStepsReached(n) => eprintln!("  Max {} steps", n),
            LoopEvent::StreamToken(_) => {} // only from run_loop_stream
        }
    }).await.unwrap();

    println!("Finished in {} steps", steps);
}
```

## Streaming (TUI / progressive output)

For streaming tokens during the LLM decision phase, implement `SgrAgentStream` and use `run_loop_stream`:

```rust
use baml_agent::{SgrAgentStream, run_loop_stream};

impl SgrAgentStream for MyAgent {
    fn decide_stream<T>(
        &self,
        messages: &[MyMsg],
        mut on_token: T,
    ) -> impl Future<Output = Result<StepDecision<MyActionUnion>, String>> + Send
    where
        T: FnMut(&str) + Send,
    {
        async move {
            let stream = B.MyDecideFunction
                .with_client_registry(&self.registry)
                .stream(&baml_msgs)
                .await
                .map_err(|e| e.to_string())?;

            while let Some(partial) = stream.next().await {
                on_token(&partial.raw_text);
            }

            let result = stream.get_final_response().await.map_err(|e| e.to_string())?;
            Ok(StepDecision { /* ... */ })
        }
    }
}

// Use run_loop_stream instead of run_loop
let steps = run_loop_stream(&agent, &mut session, &loop_config, |event| {
    match event {
        LoopEvent::StreamToken(token) => print!("{}", token), // live output
        // ... same as above
    }
}).await.unwrap();
```

## Trait hierarchy

```
SgrAgent                          SgrAgentStream : SgrAgent
  decide()                          decide_stream(on_token)
  execute()
  action_signature()               (inherits all from SgrAgent)
  action_category()  [default]

run_loop(impl SgrAgent)           run_loop_stream(impl SgrAgentStream)
  calls decide()                    calls decide_stream()
  no StreamToken events             emits StreamToken events
  3-tier loop detection             3-tier loop detection
```

- **CLI agents** — `SgrAgent` only, `run_loop()`. No streaming needed for autonomous CLI.
- **TUI agents** — implement both `SgrAgent` + `SgrAgentStream`. Headless mode uses `run_loop_stream()`. TUI uses `step_stream()` + manual loop with `process_step()`.

## Session persistence

`Session<M>` saves every message to a JSONL file using UUID v7 session IDs (time-sortable). Messages use typed structs (`EntryType`, `MessageBody`, `MessageContent`, `ContentBlock`) with a Claude Code compatible format: user/system entries have plain string content, assistant/tool entries use content blocks arrays. Supports resume:

```rust
// New session
let session = Session::<MyMsg>::new(".sessions", 60);

// Resume specific session
let session = Session::<MyMsg>::resume(&path, ".sessions", 60);

// Resume most recent
let session = Session::<MyMsg>::resume_last(".sessions", 60);

// Auto-trim when history exceeds max (preserves system messages)
let trimmed = session.trim(); // returns number of trimmed messages
```

### Session management

List and search past sessions without loading full message history:

```rust
use baml_agent::session::{list_sessions, SessionMeta};

// List all sessions (newest first)
let sessions: Vec<SessionMeta> = list_sessions(".sessions");
for s in &sessions {
    println!("[{}] {} ({} msgs, {}B)",
        s.created, s.topic, s.message_count, s.size_bytes);
}

// Resume by selection
let picked = &sessions[0];
let session = Session::<MyMsg>::resume(&picked.path, ".sessions", 60);
```

`SessionMeta` fields:
- `path` — JSONL file path
- `created` — unix timestamp (extracted from UUID v7 in filename)
- `message_count` — number of messages (line count)
- `topic` — first user message (truncated to 120 chars)
- `size_bytes` — file size

### Fuzzy search (feature `search`)

Requires `baml-agent = { features = ["search"] }` (adds `nucleo-matcher` dep):

```rust
use baml_agent::session::search_sessions;

// Fuzzy match on topic (first user message)
let results = search_sessions(".sessions", "fix bug");
for (score, meta) in &results {
    println!("[score={}] {}", score, meta.topic);
}
```

## System prompt template

```rust
use baml_agent::prompt::build_system_prompt;

let prompt = build_system_prompt(
    "sales assistant for B2B SaaS",
    "- search_crm: find contacts by name or company\n- send_email: compose and send email\n- schedule_call: book a meeting",
    "Always be polite. Never share internal pricing. Follow up within 24h.",
);
// Use in BAML: replace {output_format} with {{ ctx.output_format }}
```

## Provider config

`AgentConfig::vertex_from_env()` reads `GOOGLE_CLOUD_PROJECT` and sets up:
- `vertex` — Gemini 3.1 Flash Lite (primary)
- `vertex_fallback` — Gemini 3 Flash
- `local` — Ollama llama3.2 at localhost:11434

Custom providers:

```rust
let mut config = AgentConfig::vertex_from_env()?;
config.add_provider("openai", ProviderConfig {
    provider_type: "openai".into(),
    model: "gpt-4o-mini".into(),
    api_key_env_var: Some("OPENAI_API_KEY".into()),
    base_url: None,
    location: None,
    project_id: None,
});
config.default_provider = "openai".into();
```

## Stateful executors

If `execute()` needs mutable state (MCP connections, DB handles), use interior mutability:

```rust
struct MyAgent {
    registry: ClientRegistry,
    mcp: Mutex<McpClient>,  // interior mutability
}

impl SgrAgent for MyAgent {
    async fn execute(&self, action: &Action) -> Result<ActionResult, String> {
        let mut mcp = self.mcp.lock().await;
        let result = mcp.call_tool(&action.tool_name, &action.args).await?;
        Ok(ActionResult { output: result, done: false })
    }
}
```

## STAR reasoning framework

The agent loop uses STAR (Situation → Task → Action → Result) as the structured reasoning pattern. `StepDecision` maps directly:

| STAR | Field | What the LLM fills |
|------|-------|---------------------|
| **S** — Situation | `situation` | Current state, what's done, what blocks progress |
| **T** — Task | `task` | 1-5 remaining steps, first = execute now |
| **A** — Action | `actions` | Tool calls to run (parallel if independent) |
| **R** — Result | `completed` | `true` only when goal is fully achieved |

### BAML field design rules (critical for union actions)

**All optional fields in task classes MUST be `string | null`, not `string`.**

LLMs (Gemini, GPT, Claude) struggle to generate union-typed arrays when task classes have many required fields. If a task has 6 required `string` fields but only 2 are relevant for the current operation, the model often **skips the entire `next_actions` array** rather than filling irrelevant fields with empty strings.

```baml
// BAD — model skips next_actions because it can't fill all required fields
class ProjectTask {
  task "project_operation" @stream.not_null
  operation "create" | "open" | "add_files"
  project_path string
  input_path string        // required but unused for "create"
  meta_key string          // required but unused for "create"
  meta_value string        // required but unused for "create"
}

// GOOD — model can emit the action with only relevant fields
class ProjectTask {
  task "project_operation" @stream.not_null
  operation "create" | "open" | "add_files"
  project_path string @description("Path to .l2f project file")
  input_path string | null @description("File path for add_files")
  meta_key string | null @description("Key for set_meta/get_meta")
  meta_value string | null @description("Value for set_meta")
}
```

**Symptoms of this bug:** `current_state` and `plan` are populated correctly, but `next_actions` is always `[]`. The agent describes what it wants to do but never emits tool calls. Affects all models (Gemini Flash Lite, Flash, Pro, GPT-4o).

**The empty-actions guard** in `process_step()` detects this and nudges the model with a system message: "You MUST emit at least one tool call." After `loop_abort_threshold` empty steps, the loop aborts.

### Prompt tips for STAR

Place this near `{{ ctx.output_format }}` in your BAML prompt:

```
CRITICAL: The `next_actions` array MUST contain at least one action.
Never return an empty array. Pick the tool for the next phase.
```

Define a phase-based workflow (ORIENT → PROJECT → ANALYZE → ...) so the model always knows which tool to emit next. Add "NEVER go back to a completed phase" to prevent loops.

## Loop detection (3-tier)

`LoopDetector` catches three types of agent loops, each tracked independently:

| Tier | Signal | Catches | Example |
|------|--------|---------|---------|
| **1. Exact** | Identical `action_signature()` | Trivial loops (same tool, same args) | `inspect:/path` × 6 |
| **2. Category** | Normalized `action_category()` | Semantic loops (same intent, different syntax) | `rg -n 'TODO' src/` vs `grep -rn "TODO" src/` |
| **3. Output** | Identical tool output (hash) | Stagnation (different commands, same result) | "No matches found" × 4 |

Thresholds: warns at `⌈abort/2⌉`, aborts at `abort_threshold`. Default: warn at 3, abort at 6.

### How it works in the loop

```
decide() → action_signature() + action_category()
         → check_with_category(sig, cat)  ← Tier 1+2
         → if Warning: inject "try different approach" system message
         → if Abort: terminate loop

execute() → tool output
          → record_output(output)          ← Tier 3
          → if Warning: inject "result is definitive" system message
          → if Abort: terminate loop
```

All three tiers are automatic — `process_step()` handles everything. No per-project wiring needed.

### Signature normalization (`normalize_signature`)

Tier 2 uses `normalize_signature()` to collapse bash command variations into a canonical form:

```rust
use baml_agent::normalize_signature;

// All normalize to "bash-search:TODO|FIXME crates/src"
normalize_signature("bash:rg -n 'TODO|FIXME' crates/src/");
normalize_signature("bash:rg -Hn \"TODO|FIXME\" crates/src/");
normalize_signature("bash:grep -rnE 'TODO|FIXME' crates/src/ || echo 'not found'");

// Non-bash signatures pass through unchanged
normalize_signature("inspect:/path/video.mp4"); // → "inspect:/path/video.mp4"
```

Rules for bash signatures:
1. Strip fallback chains (`||`, `&&`, `;`, `|`)
2. Remove flags (`-n`, `-i`, `--long-flag`)
3. Strip quotes and trailing slashes from args
4. Search tools (`rg`, `grep`, `ag`, `ack`) → `bash-search:args`
5. Other commands → `bash:cmd:args`

### Custom category (optional)

Override `action_category()` on `SgrAgent` for project-specific normalization:

```rust
impl SgrAgent for MyAgent {
    // Default: normalize_signature(&action_signature(action))
    // Override for domain-specific collapsing:
    fn action_category(action: &MyAction) -> String {
        match action {
            // Collapse all analysis variants to one category
            MyAction::Analyze(t) => format!("analyze:{}", t.input_path),
            _ => normalize_signature(&Self::action_signature(action)),
        }
    }
}
```

## Helpers (`helpers` module)

Reusable utilities extracted from real agent implementations. Import directly or via re-exports:

```rust
use baml_agent::{norm, norm_owned, action_result_json, action_result_from, action_result_done, truncate_json_array, load_manifesto};
```

### BAML enum normalization

BAML generates Rust enum variants with a `K` prefix (`Ksystem`, `Kdefault`). `norm()` strips it:

```rust
use baml_agent::norm;

let op = norm("Kdefault"); // → "default"
let role = norm("Ksystem"); // → "system"
let clean = norm("already_clean"); // → "already_clean"

// norm_owned() takes owned String (convenience for format!("{:?}", variant))
use baml_agent::norm_owned;
let op = norm_owned(format!("{:?}", t.operation)); // → "create"
```

### ActionResult builders

Every `execute()` arm follows the same pattern: call IO → wrap JSON → ActionResult. Helpers eliminate boilerplate:

```rust
use baml_agent::{action_result_from, action_result_json, action_result_done};

// From Result<Value, E> — wraps error in {"error": "..."}
async fn execute(&self, action: &Action) -> Result<ActionResult, String> {
    match action {
        Action::FsTask(t) => {
            let io_task = FsTask { operation: norm_owned(format!("{:?}", t.op)), .. };
            Ok(action_result_from(execute_fs_task(&io_task)))
        }
        // From a Value directly (non-terminal)
        Action::AudioTask(t) => {
            let mut res = execute_audio(&t)?;
            truncate_json_array(&mut res, "beats", 10);
            Ok(action_result_json(&res))
        }
        // Terminal action (signals loop completion)
        Action::Finish(t) => Ok(action_result_done(&t.summary)),
    }
}
```

### JSON array truncation

Keep context window manageable by truncating large arrays in tool results:

```rust
use baml_agent::truncate_json_array;

let mut res = serde_json::json!({"segments": [/* 500 items */], "beats": [/* 200 items */]});
truncate_json_array(&mut res, "segments", 10); // keeps 10 + "... showing 10 of 500 total"
truncate_json_array(&mut res, "beats", 10);
```

### AgentContext — layered memory system

Two loading modes that merge into a single system message:

#### 1. Agent home dir (`load`)

Each agent has a configurable home dir (e.g. `.my-agent/`). All files are optional — use only what your agent needs:

| File | Label | What |
|------|-------|------|
| `SOUL.md` | Soul | Who the agent is: values, boundaries, tone (user-customizable persona) |
| `IDENTITY.md` | Identity | Name, role, stack, domain (optional — prefer baking into BAML prompt) |
| `MANIFESTO.md` | Manifesto | Dev principles, harness engineering (optional) |
| `RULES.md` | Rules | Coding rules, workflow constraints (optional — prefer baking into BAML prompt) |
| `MEMORY.md` | Memory (user notes) | Human-editable free-form notes (semi-manual) |
| `MEMORY.jsonl` | Memory (learned) | Typed agent memory — auto-written, auto-GC'd |
| `context/*.md` | (filename) | User-extensible extras |

**Recommended pattern**: Bake domain logic (pipeline phases, tools, rules) into the BAML prompt. Use home dir files only for user-customizable content (persona, preferences, learned patterns). This prevents users from accidentally breaking agent behavior by editing logic files.

#### 2. Project dir (`load_project`) — Claude Code compatible

| Priority | File | Scope |
|----------|------|-------|
| 1 | `AGENTS.md` > `CLAUDE.md` > `.claude/CLAUDE.md` | Project instructions (git) |
| 2 | `AGENTS.local.md` > `CLAUDE.local.md` | Local instructions (gitignored) |
| 3 | `.agents/rules/*.md` > `.claude/rules/*.md` | Rules by topic |

Supports `@path/to/file` imports (Claude Code compatible, recursive up to depth 5).

```rust
use baml_agent::AgentContext;

// Load agent-specific context + project context
let mut ctx = AgentContext::load(".my-agent");
ctx.merge(AgentContext::load_project(Path::new(".")));

// Inject into session
if let Some(msg) = ctx.to_system_message() {
    session.push(Role::system(), msg);
}

// With token budget (drops low-priority parts first)
if let Some(msg) = ctx.to_system_message_with_budget(8000) {
    session.push(Role::system(), msg);
}
```

#### Typed memory (MEMORY.jsonl)

Agent writes structured entries via a MemoryTask tool (defined in each agent's BAML schema):

```jsonl
{"category":"preference","section":"User Rules","content":"Always use film profile for travel videos","confidence":"confirmed","created":1772700000}
{"category":"pattern","section":"Scoring","content":"Garbage filter 0.3 works better for short clips","confidence":"tentative","created":1772700100}
{"category":"decision","section":"Build System","content":"Use cargo, not make","confidence":"confirmed","created":1772700200}
```

Two confidence levels:
- `confirmed` — user-confirmed rules (via `store_rule`). Live forever.
- `tentative` — agent-learned patterns (via `learn`). Auto-expire after 7 days if not confirmed.

Loaded into system message as:
```
### Build System
- [✓|decision] Use cargo

### Testing
- [?|pattern] Run check before test
```

**Garbage collection**: tentative entries older than 7 days are auto-removed on load. Confirmed entries live forever.

**Token budget priority** (highest kept, lowest dropped first):

| Priority | Label | Droppable? |
|----------|-------|------------|
| 10 | Soul | Never |
| 9 | Memory (user notes) | Never |
| 8 | Identity, Rules | Yes |
| 7 | Project/Local Instructions | Yes |
| 6 | Memory (learned) | Yes |
| 5 | Manifesto | Yes |
| 3 | context/* extras, rules/* | Yes (first to go) |

### Agent manifesto loader (legacy)

Simple loader for `agent.md` / `.director/agent.md` in CWD. Use `AgentContext` for new agents.

```rust
use baml_agent::{load_manifesto, load_manifesto_from};
let manifesto = load_manifesto(); // from CWD
```

## Tests

```bash
cargo test -p baml-agent
# 81 tests: session (typed structs, UUID v7, format, store, meta),
# trimming, 3-tier loop detection, agent loop, streaming,
# empty actions guard, helpers, AgentContext, memory GC,
# token budget, @import, project loading
```
