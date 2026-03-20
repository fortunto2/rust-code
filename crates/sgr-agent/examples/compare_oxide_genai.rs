//! Compare openai-oxide vs genai vs async-openai on GPT-5.4.
//!
//! Sends identical prompts through all three backends, measures latency + tokens,
//! exports traces to Phoenix (localhost:6006) for visual comparison.
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

/// Structured output schema — same for all backends.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct CityInfo {
    /// City name
    name: String,
    /// Country
    country: String,
    /// Population (approximate)
    population: u64,
    /// Top 3 landmarks
    landmarks: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Init telemetry → Phoenix
    let guard = sgr_agent::init_telemetry("/tmp/compare-logs", "compare");

    let model = "gpt-5.4";
    // All backends use Responses API for fair comparison
    // genai: "openai_resp::" prefix routes to /responses instead of /chat/completions
    let genai_model = format!("openai_resp::{model}");
    let config = LlmConfig::auto(&genai_model)
        .temperature(0.3)
        .max_tokens(500);
    let oxide_config = LlmConfig::auto(model).temperature(0.3).max_tokens(500);

    println!("=== Comparing oxide vs genai vs async-openai (all Responses API) ===");
    println!("Model: {model}\n");

    // ── Test 1: Plain text completion ──
    println!("--- Test 1: Plain text ---");
    let messages = vec![
        Message::system("You are a helpful assistant. Be concise."),
        Message::user("What is the capital of Kazakhstan? One sentence."),
    ];

    // genai
    let llm = Llm::new(&config);
    let t0 = Instant::now();
    let genai_result = llm.generate(&messages).await?;
    let genai_ms = t0.elapsed().as_millis();
    println!("  genai        ({genai_ms:>4}ms): {genai_result}");

    // oxide
    let oxide = OxideClient::from_config(&oxide_config)?;
    let t0 = Instant::now();
    let oxide_result = oxide.complete(&messages).await?;
    let oxide_ms = t0.elapsed().as_millis();
    println!("  oxide        ({oxide_ms:>4}ms): {oxide_result}");

    // async-openai
    let aoai = AsyncOpenAIClient::from_config(&oxide_config)?;
    let t0 = Instant::now();
    let aoai_result = aoai.complete(&messages).await?;
    let aoai_ms = t0.elapsed().as_millis();
    println!("  async-openai ({aoai_ms:>4}ms): {aoai_result}");

    // ── Test 2: Structured output ──
    println!("\n--- Test 2: Structured output (CityInfo) ---");
    let messages = vec![
        Message::system("You are a geography expert."),
        Message::user("Tell me about Tokyo."),
    ];
    let schema = sgr_agent::response_schema_for::<CityInfo>();

    // genai (structured_call via LlmClient trait)
    let t0 = Instant::now();
    let (genai_parsed, _, genai_raw) = llm.structured_call(&messages, &schema).await?;
    let genai_ms = t0.elapsed().as_millis();
    if let Some(val) = &genai_parsed {
        let city: CityInfo = serde_json::from_value(val.clone())?;
        println!(
            "  genai        ({genai_ms:>4}ms): {} — pop {}, landmarks: {:?}",
            city.name, city.population, city.landmarks
        );
    } else {
        println!("  genai        ({genai_ms:>4}ms): parse failed, raw: {genai_raw}");
    }

    // oxide (structured_call via LlmClient trait)
    let t0 = Instant::now();
    let (oxide_parsed, _, oxide_raw) = oxide.structured_call(&messages, &schema).await?;
    let oxide_ms = t0.elapsed().as_millis();
    if let Some(val) = &oxide_parsed {
        let city: CityInfo = serde_json::from_value(val.clone())?;
        println!(
            "  oxide        ({oxide_ms:>4}ms): {} — pop {}, landmarks: {:?}",
            city.name, city.population, city.landmarks
        );
    } else {
        println!("  oxide        ({oxide_ms:>4}ms): parse failed, raw: {oxide_raw}");
    }

    // async-openai (structured_call via LlmClient trait)
    let t0 = Instant::now();
    let (aoai_parsed, _, aoai_raw) = aoai.structured_call(&messages, &schema).await?;
    let aoai_ms = t0.elapsed().as_millis();
    if let Some(val) = &aoai_parsed {
        let city: CityInfo = serde_json::from_value(val.clone())?;
        println!(
            "  async-openai ({aoai_ms:>4}ms): {} — pop {}, landmarks: {:?}",
            city.name, city.population, city.landmarks
        );
    } else {
        println!("  async-openai ({aoai_ms:>4}ms): parse failed, raw: {aoai_raw}");
    }

    // ── Test 3: Multi-turn via previous_response_id ──
    println!("\n--- Test 3: Multi-turn via previous_response_id ---");

    // genai (already on Responses API via openai_resp:: prefix)
    let dummy_tool = sgr_agent::tool::ToolDef {
        name: "noop".into(),
        description: "No-op".into(),
        parameters: serde_json::json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false}),
    };
    let t0 = Instant::now();
    let (_, genai_resp_id) = llm
        .tools_call_stateful(
            &[Message::user("My name is Rustam.")],
            &[dummy_tool.clone()],
            None,
        )
        .await?;
    let (_, _) = llm
        .tools_call_stateful(
            &[Message::user("What is my name?")],
            &[dummy_tool.clone()],
            genai_resp_id.as_deref(),
        )
        .await?;
    let genai_ms = t0.elapsed().as_millis();
    println!(
        "  genai        ({genai_ms:>4}ms): response_id={:?}",
        genai_resp_id.as_deref().unwrap_or("none")
    );

    // oxide
    let t0 = Instant::now();
    let r1 = oxide
        .complete(&[Message::user("My name is Rustam.")])
        .await?;
    let r2 = oxide.complete(&[Message::user("What is my name?")]).await?;
    let oxide_ms = t0.elapsed().as_millis();
    println!("  oxide        ({oxide_ms:>4}ms): Turn 1: {r1} | Turn 2: {r2}");

    // async-openai
    let t0 = Instant::now();
    let r1 = aoai
        .complete(&[Message::user("My name is Rustam.")])
        .await?;
    let r2 = aoai.complete(&[Message::user("What is my name?")]).await?;
    let aoai_ms = t0.elapsed().as_millis();
    println!("  async-openai ({aoai_ms:>4}ms): Turn 1: {r1} | Turn 2: {r2}");

    // ── Test 4: Function calling ──
    println!("\n--- Test 4: Function calling ---");
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
    let messages = vec![
        Message::system("Use tools when needed."),
        Message::user("What's the weather in Moscow?"),
    ];

    // genai
    let t0 = Instant::now();
    let genai_calls = llm.tools_call(&messages, &[weather_tool.clone()]).await?;
    let genai_ms = t0.elapsed().as_millis();
    println!(
        "  genai        ({genai_ms:>4}ms): {} tool call(s)",
        genai_calls.len()
    );
    for tc in &genai_calls {
        println!("    -> {}({})", tc.name, tc.arguments);
    }

    // oxide
    let t0 = Instant::now();
    let oxide_calls = oxide.tools_call(&messages, &[weather_tool.clone()]).await?;
    let oxide_ms = t0.elapsed().as_millis();
    println!(
        "  oxide        ({oxide_ms:>4}ms): {} tool call(s)",
        oxide_calls.len()
    );
    for tc in &oxide_calls {
        println!("    -> {}({})", tc.name, tc.arguments);
    }

    // async-openai
    let t0 = Instant::now();
    let aoai_calls = aoai.tools_call(&messages, &[weather_tool]).await?;
    let aoai_ms = t0.elapsed().as_millis();
    println!(
        "  async-openai ({aoai_ms:>4}ms): {} tool call(s)",
        aoai_calls.len()
    );
    for tc in &aoai_calls {
        println!("    -> {}({})", tc.name, tc.arguments);
    }

    println!("\n=== Done. Check Phoenix at http://localhost:6006 ===");

    // Flush traces
    guard.shutdown();
    Ok(())
}
