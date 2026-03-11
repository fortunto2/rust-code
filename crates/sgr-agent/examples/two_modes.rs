//! Two modes demo: structured vs flexible on the same Gemini model.
//!
//! Shows that both paths produce identical parsed results.
//!
//! Run: GEMINI_API_KEY=... cargo run -p sgr-agent --example two_modes

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::gemini::GeminiClient;
use sgr_agent::types::Message;

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct NextStep {
    situation: String,
    task: String,
    actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "tool_name")]
enum Action {
    #[serde(rename = "read_file")]
    ReadFile { path: String },
    #[serde(rename = "write_file")]
    WriteFile { path: String, content: String },
    #[serde(rename = "bash")]
    Bash { command: String },
    #[serde(rename = "finish")]
    Finish { summary: String },
}

fn make_messages(schema_hint: bool) -> Vec<Message> {
    let schema =
        serde_json::to_string_pretty(&sgr_agent::response_schema_for::<NextStep>()).unwrap();

    let system = if schema_hint {
        format!(
            "You are a coding agent. Respond ONLY with valid JSON matching this schema:\n{}\n\nNo extra text.",
            schema
        )
    } else {
        "You are a coding agent. Think step by step, then provide a JSON action plan. \
         The JSON should have: situation (string), task (string), actions (array of objects \
         with tool_name being one of: read_file, write_file, bash, finish, plus relevant params)."
            .to_string()
    };

    vec![
        Message::system(system),
        Message::user("Read the file Cargo.toml and check what dependencies we have"),
    ]
}

#[tokio::main]
async fn main() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY not set");
    let model = "gemini-2.5-flash";

    let client = GeminiClient::from_api_key(&api_key, model);

    println!("=== Mode 1: STRUCTURED (responseSchema enforced) ===\n");
    let msgs = make_messages(true);
    match client.call::<NextStep>(&msgs, &[]).await {
        Ok(resp) => {
            if let Some(ref step) = resp.output {
                println!("  situation: {}", step.situation);
                println!("  task:      {}", step.task);
                println!("  actions:   {} total", step.actions.len());
                for (i, a) in step.actions.iter().enumerate() {
                    println!("    [{}] {:?}", i, a);
                }
            }
            if let Some(ref u) = resp.usage {
                println!(
                    "\n  tokens: {} in, {} out, {} total",
                    u.prompt_tokens, u.completion_tokens, u.total_tokens
                );
            }
            if let Some(ref rl) = resp.rate_limit {
                println!("  rate_limit: {}", rl.status_line());
            } else {
                println!("  rate_limit: (no headers)");
            }
        }
        Err(e) => {
            eprintln!("  ERROR: {}", e);
            if e.is_rate_limit() {
                let info = e.rate_limit_info().unwrap();
                eprintln!("  Rate limit details: {}", info.status_line());
            }
        }
    }

    println!("\n=== Mode 2: FLEXIBLE (plain text, no schema enforcement) ===\n");

    // Test both: with and without schema hint
    for (label, schema_hint) in [("with schema hint", true), ("without schema hint", false)] {
        println!("  --- {} ---", label);
        let msgs = make_messages(schema_hint);
        match client.flexible::<NextStep>(&msgs).await {
            Ok(resp) => {
                let raw = &resp.raw_text;
                let display = if raw.len() > 500 { &raw[..500] } else { raw };
                println!("  raw ({} chars):\n{}\n", raw.len(), display);

                if let Some(ref step) = resp.output {
                    println!("  PARSED OK:");
                    println!("    situation: {}", step.situation);
                    println!("    task:      {}", step.task);
                    println!("    actions:   {} total", step.actions.len());
                    for (i, a) in step.actions.iter().enumerate() {
                        println!("      [{}] {:?}", i, a);
                    }
                }
                if let Some(ref u) = resp.usage {
                    println!(
                        "  tokens: {} in, {} out, {} total",
                        u.prompt_tokens, u.completion_tokens, u.total_tokens
                    );
                }
            }
            Err(e) => {
                eprintln!("  ERROR: {}", e);
                // Try to get the raw text for debugging
                eprintln!("  (flexible parse failed — trying manual parse for debug)");

                // Make a raw call to see what the model returns
                let raw_resp = client.flexible::<serde_json::Value>(&msgs).await;
                if let Ok(resp) = raw_resp {
                    println!(
                        "  raw as Value: {}",
                        serde_json::to_string_pretty(&resp.output).unwrap_or_default()
                    );
                }
            }
        }
        println!();
    }

    println!("\n=== Summary ===");
    println!("Both modes should produce equivalent NextStep structs.");
    println!("Structured = guaranteed JSON via API. Flexible = parse from free text.");
}
