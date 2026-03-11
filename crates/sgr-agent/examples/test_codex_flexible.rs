//! Integration test: Codex proxy → flexible parser.
//!
//! Run: cargo run -p sgr-agent --example test_codex_flexible

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::flexible_parser::{parse_flexible, parse_flexible_coerced};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Answer {
    /// The answer to the question.
    answer: String,
    /// Confidence score from 0.0 to 1.0.
    confidence: f64,
    /// One-word category.
    category: String,
}

/// Minimal Codex proxy — just enough to get a text response.
async fn call_codex_text(prompt: &str) -> Result<String, String> {
    // Read auth
    let home = std::env::var("HOME").unwrap();
    let auth_path = format!("{}/.codex/auth.json", home);
    let auth_content = std::fs::read_to_string(&auth_path).map_err(|e| e.to_string())?;
    let auth_json: serde_json::Value =
        serde_json::from_str(&auth_content).map_err(|e| e.to_string())?;
    let refresh_token = auth_json["tokens"]["refresh_token"]
        .as_str()
        .ok_or("no refresh_token")?;

    // Refresh access token
    let client = reqwest::Client::new();
    let token_resp = client
        .post("https://auth.openai.com/oauth/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=refresh_token&refresh_token={}&client_id=app_EMoamEEZ73f0CkXaXp7hrann",
            refresh_token
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let token_json: serde_json::Value = token_resp.json().await.map_err(|e| e.to_string())?;
    let access_token = token_json["access_token"]
        .as_str()
        .ok_or("no access_token")?;

    // Extract account_id from JWT
    let parts: Vec<&str> = access_token.split('.').collect();
    let payload = base64_decode_url(parts[1])?;
    let jwt: serde_json::Value = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    let account_id = jwt["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .ok_or("no account_id")?;

    // Call Codex Responses API
    let schema = sgr_agent::response_schema_for::<Answer>();
    let system_prompt = format!(
        "You are a helpful assistant. Always respond with valid JSON matching this schema:\n{}",
        serde_json::to_string_pretty(&schema).unwrap()
    );

    let body = serde_json::json!({
        "model": "gpt-5.3-codex",
        "store": false,
        "stream": true,
        "instructions": system_prompt,
        "input": [
            {"role": "user", "content": prompt}
        ],
        "text": {"verbosity": "medium"}
    });

    let resp = client
        .post("https://chatgpt.com/backend-api/codex/responses")
        .header("Authorization", format!("Bearer {}", access_token))
        .header("chatgpt-account-id", account_id)
        .header("OpenAI-Beta", "responses=experimental")
        .header("Content-Type", "application/json")
        .header("accept", "text/event-stream")
        .body(serde_json::to_string(&body).unwrap())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        return Err(format!(
            "Codex API error {}: {}",
            status,
            &text[..text.len().min(300)]
        ));
    }

    // Parse SSE
    let text = resp.text().await.map_err(|e| e.to_string())?;
    let mut output = String::new();

    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                break;
            }
            if let Ok(event) = serde_json::from_str::<serde_json::Value>(data) {
                let event_type = event["type"].as_str().unwrap_or("");
                if event_type == "response.output_text.done" {
                    if let Some(t) = event["text"].as_str() {
                        output = t.to_string();
                    }
                }
                if event_type == "response.completed" && output.is_empty() {
                    if let Some(outputs) = event["response"]["output"].as_array() {
                        for o in outputs {
                            if let Some(contents) = o["content"].as_array() {
                                for c in contents {
                                    if let Some(t) = c["text"].as_str() {
                                        output.push_str(t);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(output)
}

fn base64_decode_url(input: &str) -> Result<Vec<u8>, String> {
    // Pad to multiple of 4
    let padded = match input.len() % 4 {
        2 => format!("{}==", input),
        3 => format!("{}=", input),
        _ => input.to_string(),
    };
    // URL-safe base64
    let standard = padded.replace('-', "+").replace('_', "/");
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(&standard)
        .map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() {
    println!("=== Testing Codex → flexible parser ===\n");

    let prompts: &[&str] = &[
        // 1. Clean JSON expected
        "What is the capital of France? Respond with JSON containing answer, confidence (0-1), and category.",
        // 2. Might wrap in markdown
        "What is 2+2? Give me answer, confidence, and category in a JSON code block.",
        // 3. Might add chain-of-thought before JSON
        "Think step by step: what is the largest ocean? Then give JSON with answer, confidence, and category.",
    ];

    for (i, prompt) in prompts.iter().enumerate() {
        println!("━━━ Test {} ━━━", i + 1);
        println!("Prompt: {}\n", prompt);

        match call_codex_text(prompt).await {
            Ok(raw) => {
                println!("Raw response:\n{}\n", raw);

                println!("parse_flexible (strict):");
                match parse_flexible::<Answer>(&raw) {
                    Ok(result) => {
                        println!(
                            "  OK! Source: {:?}, tried: {}",
                            result.source, result.candidates_tried
                        );
                        println!("  {:?}", result.value);
                    }
                    Err(e) => {
                        println!("  FAILED: {}", e);
                        println!("\nparse_flexible_coerced:");
                        match parse_flexible_coerced::<Answer>(&raw) {
                            Ok(result) => {
                                println!(
                                    "  OK! Source: {:?}, tried: {}",
                                    result.source, result.candidates_tried
                                );
                                println!("  {:?}", result.value);
                            }
                            Err(e) => {
                                println!("  FAILED: {}", e);
                            }
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Error: {}", e);
            }
        }
        println!();
    }
}
