//! Compare openai-oxide vs genai vs async-openai vs Python on GPT-5.4.
//!
//! All backends use Responses API. Connection pre-warming for fair comparison.
//!
//! ```bash
//! OPENAI_API_KEY=sk-... \
//! OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:6006/v1/traces \
//! cargo run -p sgr-agent --example compare_oxide_genai --features "oxide,genai,async-openai-backend,telemetry"
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::Llm;
use sgr_agent::async_openai_client::AsyncOpenAIClient;
use sgr_agent::client::LlmClient;
use sgr_agent::oxide_client::OxideClient;
use sgr_agent::types::{LlmConfig, Message};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CityInfo {
    name: String,
    country: String,
    population: u64,
    landmarks: Vec<String>,
}

/// Warm up a client — establishes TLS connection + HTTP/2 negotiation.
async fn warmup(label: &str, client: &dyn LlmClient) {
    let _ = client.complete(&[Message::user("ping")]).await;
    eprintln!("[warmup] {label} ready");
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let guard = sgr_agent::init_telemetry("/tmp/compare-logs", "compare");

    let model = "gpt-5.4";
    let genai_model = format!("openai_resp::{model}");
    let genai_config = LlmConfig::auto(&genai_model)
        .temperature(0.3)
        .max_tokens(500);
    let oxide_config = LlmConfig::auto(model).temperature(0.3).max_tokens(500);

    let llm = Llm::new(&genai_config);
    let oxide = OxideClient::from_config(&oxide_config)?;
    let aoai = AsyncOpenAIClient::from_config(&oxide_config)?;

    // ── Pre-warm all connections (TLS + HTTP/2) ──
    println!("Warming up connections...");
    warmup("genai", &llm).await;
    warmup("oxide", &oxide).await;
    warmup("async-openai", &aoai).await;

    println!("\n=== Benchmark: oxide vs genai vs async-openai (all Responses API, warm) ===");
    println!("Model: {model}\n");

    // ── Test 1: Plain text ──
    println!("--- Test 1: Plain text ---");
    let msgs = vec![
        Message::system("You are a helpful assistant. Be concise."),
        Message::user("What is the capital of Kazakhstan? One sentence."),
    ];

    let t0 = Instant::now();
    let _ = oxide.complete(&msgs).await?;
    let oxide_ms = t0.elapsed().as_millis();

    let t0 = Instant::now();
    let _ = llm.generate(&msgs).await?;
    let genai_ms = t0.elapsed().as_millis();

    let t0 = Instant::now();
    let _ = aoai.complete(&msgs).await?;
    let aoai_ms = t0.elapsed().as_millis();

    println!("  oxide        ({oxide_ms:>4}ms)");
    println!("  genai        ({genai_ms:>4}ms)");
    println!("  async-openai ({aoai_ms:>4}ms)");

    // ── Test 2: Structured output ──
    println!("\n--- Test 2: Structured output ---");
    let msgs = vec![
        Message::system("You are a geography expert."),
        Message::user("Tell me about Tokyo."),
    ];
    let schema = sgr_agent::response_schema_for::<CityInfo>();

    let t0 = Instant::now();
    let (_, _, _) = oxide.structured_call(&msgs, &schema).await?;
    let oxide_ms = t0.elapsed().as_millis();

    let t0 = Instant::now();
    let (_, _, _) = llm.structured_call(&msgs, &schema).await?;
    let genai_ms = t0.elapsed().as_millis();

    let t0 = Instant::now();
    let (_, _, _) = aoai.structured_call(&msgs, &schema).await?;
    let aoai_ms = t0.elapsed().as_millis();

    println!("  oxide        ({oxide_ms:>4}ms)");
    println!("  genai        ({genai_ms:>4}ms)");
    println!("  async-openai ({aoai_ms:>4}ms)");

    // ── Test 3: Function calling ──
    println!("\n--- Test 3: Function calling ---");
    let weather_tool = sgr_agent::tool::ToolDef {
        name: "get_weather".into(),
        description: "Get current weather for a city".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "city": {"type": "string", "description": "City name"},
                "unit": {"type": "string", "enum": ["celsius", "fahrenheit"]}
            },
            "required": ["city", "unit"],
            "additionalProperties": false
        }),
    };
    let msgs = vec![
        Message::system("Use tools when needed."),
        Message::user("What's the weather in Moscow?"),
    ];

    let t0 = Instant::now();
    let _ = oxide.tools_call(&msgs, &[weather_tool.clone()]).await?;
    let oxide_ms = t0.elapsed().as_millis();

    let t0 = Instant::now();
    let _ = llm.tools_call(&msgs, &[weather_tool.clone()]).await?;
    let genai_ms = t0.elapsed().as_millis();

    let t0 = Instant::now();
    let _ = aoai.tools_call(&msgs, &[weather_tool]).await?;
    let aoai_ms = t0.elapsed().as_millis();

    println!("  oxide        ({oxide_ms:>4}ms)");
    println!("  genai        ({genai_ms:>4}ms)");
    println!("  async-openai ({aoai_ms:>4}ms)");

    // ── Test 4: Multi-turn (2 requests) ──
    println!("\n--- Test 4: Multi-turn (2 requests) ---");

    // Fresh oxide for clean multi-turn (no stale previous_response_id)
    let oxide2 = OxideClient::from_config(&oxide_config)?;
    warmup("oxide2", &oxide2).await;

    let t0 = Instant::now();
    let _ = oxide2
        .complete(&[Message::user("My name is Rustam.")])
        .await?;
    let _ = oxide2
        .complete(&[Message::user("What is my name?")])
        .await?;
    let oxide_ms = t0.elapsed().as_millis();

    let dummy_tool = sgr_agent::tool::ToolDef {
        name: "noop".into(),
        description: "No-op".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}),
    };
    let t0 = Instant::now();
    let (_, rid) = llm
        .tools_call_stateful(
            &[Message::user("My name is Rustam.")],
            &[dummy_tool.clone()],
            None,
        )
        .await?;
    let _ = llm
        .tools_call_stateful(
            &[Message::user("What is my name?")],
            &[dummy_tool],
            rid.as_deref(),
        )
        .await?;
    let genai_ms = t0.elapsed().as_millis();

    let aoai2 = AsyncOpenAIClient::from_config(&oxide_config)?;
    warmup("aoai2", &aoai2).await;
    let t0 = Instant::now();
    let _ = aoai2
        .complete(&[Message::user("My name is Rustam.")])
        .await?;
    let _ = aoai2.complete(&[Message::user("What is my name?")]).await?;
    let aoai_ms = t0.elapsed().as_millis();

    println!("  oxide        ({oxide_ms:>4}ms)");
    println!("  genai        ({genai_ms:>4}ms)");
    println!("  async-openai ({aoai_ms:>4}ms)");

    println!("\n=== Done. Phoenix: http://localhost:6006 ===");
    guard.shutdown();
    Ok(())
}
