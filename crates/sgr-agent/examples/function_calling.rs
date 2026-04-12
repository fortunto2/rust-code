//! Function calling — LLM picks which tool to call, you execute it.
//!
//! Single-turn: send prompt + tool defs → get tool calls back.
//!
//! Run: cargo run -p sgr-agent --features oxide --example function_calling

use schemars::JsonSchema;
use serde::Deserialize;
use sgr_agent::tool::tool;
use sgr_agent::types::Message;
use sgr_agent::{Llm, LlmConfig};

#[derive(Debug, Deserialize, JsonSchema)]
struct WeatherArgs {
    /// City name
    city: String,
    /// Temperature unit (celsius or fahrenheit)
    #[serde(default)]
    unit: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct CalcArgs {
    /// Mathematical expression
    expression: String,
}

#[tokio::main]
async fn main() {
    let llm = Llm::new(&LlmConfig::auto("gpt-4o-mini"));

    let tools = vec![
        tool::<WeatherArgs>("get_weather", "Get current weather for a city"),
        tool::<CalcArgs>("calculate", "Evaluate a math expression"),
    ];

    let messages = vec![Message::user("What's the weather in Tokyo?")];

    match llm.tools_call_stateful(&messages, &tools, None).await {
        Ok((tool_calls, _response_id)) => {
            for tc in &tool_calls {
                println!("Tool: {}", tc.name);
                println!("Args: {}", tc.arguments);

                match tc.name.as_str() {
                    "get_weather" => {
                        let args: WeatherArgs =
                            serde_json::from_value(tc.arguments.clone()).unwrap();
                        println!(
                            "→ Fetching weather for {} ({})",
                            args.city,
                            args.unit.as_deref().unwrap_or("celsius")
                        );
                    }
                    "calculate" => {
                        let args: CalcArgs = serde_json::from_value(tc.arguments.clone()).unwrap();
                        println!("→ Computing: {}", args.expression);
                    }
                    other => println!("→ Unknown tool: {}", other),
                }
            }
        }
        Err(e) => eprintln!("Error: {}", e),
    }
}
