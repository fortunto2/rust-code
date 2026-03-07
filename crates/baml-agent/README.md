# baml-agent

Shared Rust crate for building BAML-powered SGR (Schema-Guided Reasoning) agents.

**Path:** `~/startups/shared/rust-code/crates/baml-agent`

Used by: [video-analyzer](~/startups/active/life2film/video-analyzer) (via symlink), [rust-code](~/startups/shared/rust-code), [epiphan-voice-ai](~/startups/active/epiphan-voice-ai).

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
| `session` | `Session<M>`, `AgentMessage`, `MessageRole` — JSONL persistence, history trimming |
| `loop_detect` | `LoopDetector`, `LoopStatus` — detects repeated actions, warns then aborts |
| `agent_loop` | `SgrAgent`, `SgrAgentStream`, `run_loop`, `run_loop_stream` — the core agent loop |
| `prompt` | `BASE_SYSTEM_PROMPT`, `build_system_prompt()` — system prompt template |

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
    fn from_str(s: &str) -> Option<Self> { /* match s */ }
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
            state: decision.current_state,
            plan: decision.plan,
            completed: decision.task_completed,
            actions: decision.next_actions,
        })
    }

    async fn execute(&self, action: &MyActionUnion) -> Result<ActionResult, String> {
        // Execute the action, return output text + done flag
        match action {
            MyActionUnion::SearchTask(t) => {
                let result = do_search(&t.query);
                Ok(ActionResult { output: result, done: false })
            }
            MyActionUnion::FinishTask(t) => {
                Ok(ActionResult { output: t.summary.clone(), done: true })
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
    let mut session = Session::<MyMsg>::new(".sessions", 60);
    session.push(MyRole::user(), "Find competitors for my SaaS idea".into());

    // Build agent and run
    let agent = MyAgent { registry: reg.0 };
    let loop_config = LoopConfig { max_steps: 25, loop_abort_threshold: 6 };

    let steps = run_loop(&agent, &mut session, &loop_config, |event| {
        match event {
            LoopEvent::StepStart(n) => println!("\n[Step {}]", n),
            LoopEvent::Decision { state, plan } => {
                println!("State: {}", state);
                for (i, s) in plan.iter().enumerate() { println!("  {}. {}", i+1, s); }
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
  action_signature()

run_loop(impl SgrAgent)           run_loop_stream(impl SgrAgentStream)
  calls decide()                    calls decide_stream()
  no StreamToken events             emits StreamToken events
```

- **va-agent** — `SgrAgent` only, `run_loop()`. No streaming needed for autonomous CLI.
- **rc-cli headless** — `SgrAgent` + `SgrAgentStream`, `run_loop_stream()`. Streams tokens.
- **rc-cli TUI** — `SgrAgentStream` + `run_loop_stream()` with `StreamToken` → TUI render.

## Session persistence

`Session<M>` saves every message to a JSONL file. Supports resume:

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

## Tests

```bash
cargo test -p baml-agent
# 14 tests: session CRUD, trimming, loop detection, agent loop, streaming
```
