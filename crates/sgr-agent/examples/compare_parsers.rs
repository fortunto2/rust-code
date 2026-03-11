//! Compare BAML vs sgr-agent flexible parser on real Codex responses.
//!
//! Sends identical requests through Codex proxy, collects raw text,
//! tests both parsers on the same data.
//!
//! Run: cargo run -p sgr-agent --example compare_parsers

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::flexible_parser::{parse_flexible, parse_flexible_coerced};

// --- Schema: simulates a simplified agent NextStep ---

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct NextStep {
    /// Current situation assessment.
    situation: String,
    /// What to do next.
    task: String,
    /// Actions to execute.
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

// --- Codex client (reused from test_codex_flexible) ---

async fn call_codex_raw(system: &str, user: &str) -> Result<String, String> {
    let home = std::env::var("HOME").unwrap();
    let auth_path = format!("{}/.codex/auth.json", home);
    let content = std::fs::read_to_string(&auth_path).map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    let refresh = json["tokens"]["refresh_token"]
        .as_str()
        .ok_or("no refresh_token")?;

    let client = reqwest::Client::new();
    let token_resp = client
        .post("https://auth.openai.com/oauth/token")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(format!(
            "grant_type=refresh_token&refresh_token={}&client_id=app_EMoamEEZ73f0CkXaXp7hrann",
            refresh
        ))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    let tj: serde_json::Value = token_resp.json().await.map_err(|e| e.to_string())?;
    let access = tj["access_token"].as_str().ok_or("no access_token")?;

    let parts: Vec<&str> = access.split('.').collect();
    let payload = base64_decode_url(parts[1])?;
    let jwt: serde_json::Value = serde_json::from_slice(&payload).map_err(|e| e.to_string())?;
    let account_id = jwt["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .ok_or("no account_id")?;

    let body = serde_json::json!({
        "model": "gpt-5.3-codex",
        "store": false,
        "stream": true,
        "instructions": system,
        "input": [{"role": "user", "content": user}],
        "text": {"verbosity": "medium"}
    });

    let resp = client
        .post("https://chatgpt.com/backend-api/codex/responses")
        .header("Authorization", format!("Bearer {}", access))
        .header("chatgpt-account-id", account_id)
        .header("OpenAI-Beta", "responses=experimental")
        .header("Content-Type", "application/json")
        .header("accept", "text/event-stream")
        .body(serde_json::to_string(&body).unwrap())
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !resp.status().is_success() {
        let s = resp.status();
        let t = resp.text().await.unwrap_or_default();
        return Err(format!("API error {}: {}", s, &t[..t.len().min(300)]));
    }

    let text = resp.text().await.map_err(|e| e.to_string())?;
    let mut output = String::new();
    for line in text.lines() {
        if let Some(data) = line.strip_prefix("data: ") {
            if data == "[DONE]" {
                break;
            }
            if let Ok(ev) = serde_json::from_str::<serde_json::Value>(data) {
                let et = ev["type"].as_str().unwrap_or("");
                if et == "response.output_text.done" {
                    if let Some(t) = ev["text"].as_str() {
                        output = t.to_string();
                    }
                }
                if et == "response.completed" && output.is_empty() {
                    if let Some(outs) = ev["response"]["output"].as_array() {
                        for o in outs {
                            if let Some(cs) = o["content"].as_array() {
                                for c in cs {
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
    let padded = match input.len() % 4 {
        2 => format!("{}==", input),
        3 => format!("{}=", input),
        _ => input.to_string(),
    };
    let standard = padded.replace('-', "+").replace('_', "/");
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(&standard)
        .map_err(|e| e.to_string())
}

// --- Test scenarios ---

struct TestCase {
    name: &'static str,
    system: String,
    user: &'static str,
}

fn test_cases() -> Vec<TestCase> {
    let schema =
        serde_json::to_string_pretty(&sgr_agent::response_schema_for::<NextStep>()).unwrap();

    vec![
        TestCase {
            name: "1. Clean JSON (with schema in prompt)",
            system: format!(
                "You are a coding agent. Respond ONLY with valid JSON matching this schema:\n{}\n\nDo NOT include any text outside JSON.",
                schema
            ),
            user: "Read the file src/main.rs",
        },
        TestCase {
            name: "2. Markdown allowed (natural response)",
            system: format!(
                "You are a coding agent. When responding, provide your structured response as JSON matching this schema:\n{}\n\nYou may wrap the JSON in a code block.",
                schema
            ),
            user: "Check what dependencies are in Cargo.toml",
        },
        TestCase {
            name: "3. Chain-of-thought (think then act)",
            system: format!(
                "You are a coding agent. Think step by step, then provide a JSON action plan matching this schema:\n{}",
                schema
            ),
            user: "Fix the compilation error in src/lib.rs — the function foo() is missing a return type",
        },
        TestCase {
            name: "4. Minimal prompt (no schema hint)",
            system: "You are a coding agent. Respond with JSON containing: situation (string), task (string), actions (array of objects with tool_name and params).".into(),
            user: "Create a new file called hello.rs with a hello world program",
        },
    ]
}

#[tokio::main]
async fn main() {
    println!("╔══════════════════════════════════════════════════╗");
    println!("║  BAML vs sgr-agent flexible parser comparison   ║");
    println!("║  Backend: Codex (gpt-5.3-codex, text-only)      ║");
    println!("╚══════════════════════════════════════════════════╝\n");

    let cases = test_cases();

    let mut pass_strict = 0;
    let mut pass_coerced = 0;
    let mut total = 0;

    for case in &cases {
        println!("━━━ {} ━━━", case.name);
        println!("User: {}\n", case.user);

        match call_codex_raw(&case.system, case.user).await {
            Ok(raw) => {
                total += 1;
                // Show raw (truncated)
                let display = if raw.len() > 300 { &raw[..300] } else { &raw };
                println!("Raw ({} chars):\n{}\n", raw.len(), display);

                // Test 1: parse_flexible (strict serde)
                print!("  parse_flexible:         ");
                match parse_flexible::<NextStep>(&raw) {
                    Ok(r) => {
                        pass_strict += 1;
                        println!("✓ {:?} (tried {})", r.source, r.candidates_tried);
                        println!("    situation: {}", truncate(&r.value.situation, 80));
                        println!("    actions: {}", r.value.actions.len());
                    }
                    Err(e) => println!("✗ {} candidates failed", e.candidates.len()),
                }

                // Test 2: parse_flexible_coerced (with type coercion)
                print!("  parse_flexible_coerced: ");
                match parse_flexible_coerced::<NextStep>(&raw) {
                    Ok(r) => {
                        pass_coerced += 1;
                        println!("✓ {:?} (tried {})", r.source, r.candidates_tried);
                    }
                    Err(e) => println!("✗ {} candidates failed", e.candidates.len()),
                }
            }
            Err(e) => {
                println!("  ERROR: {}", e);
            }
        }
        println!();
    }

    println!("═══ Results ═══");
    println!("  parse_flexible (strict):  {}/{}", pass_strict, total);
    println!("  parse_flexible_coerced:   {}/{}", pass_coerced, total);
    println!();
    println!("Note: BAML parses the same text via its own jsonish SAP engine.");
    println!(
        "If sgr-agent scores {}/{}, it can fully replace BAML for text-only providers.",
        total, total
    );
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..s.floor_char_boundary(max)])
    }
}
