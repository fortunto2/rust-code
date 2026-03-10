//! Integration test: SGR backend on real Gemini responses.
//!
//! Tests the full pipeline: prompt → Gemini API → flexible parse → typed NextStep.
//! Uses sgr-agent directly (rc-cli is a binary crate, no lib target).
//!
//! Run: cargo run -p rust-code --example test_sgr_backend

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct NextStep {
    situation: String,
    task: Vec<String>,
    actions: Vec<Action>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "tool_name")]
enum Action {
    #[serde(rename = "read_file")]
    ReadFile { path: String },
    #[serde(rename = "write_file")]
    WriteFile { path: String, content: String },
    #[serde(rename = "edit_file")]
    EditFile { path: String, old_string: String, new_string: String },
    #[serde(rename = "bash")]
    Bash { command: String },
    #[serde(rename = "finish")]
    Finish { summary: String },
}

fn tool_name(action: &Action) -> &str {
    match action {
        Action::ReadFile { .. } => "read_file",
        Action::WriteFile { .. } => "write_file",
        Action::EditFile { .. } => "edit_file",
        Action::Bash { .. } => "bash",
        Action::Finish { .. } => "finish",
    }
}

const SYSTEM: &str = "You are an AI coding agent. You have tools: read_file, write_file, edit_file, bash, finish. \
    Always respond with JSON containing situation (string), task (string[]), and actions (array of tool objects with tool_name field).";

fn main() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        println!("=== SGR Backend Integration Test ===\n");

        let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY required");

        let prompts: Vec<(&str, &str, &str)> = vec![
            ("Read a file", "Read the file src/main.rs and tell me what it does.", "read_file"),
            ("Run a command", "Run `ls -la` and show me the output.", "bash"),
            ("Edit a file", "Fix the typo in src/lib.rs: change 'teh' to 'the'.", "edit_file"),
            ("Multi-action", "Read Cargo.toml and then run cargo test.", "read_file"),
            ("Finish task", "Everything looks good. Summarize what was done and finish.", "finish"),
        ];

        let config = sgr_agent::ProviderConfig::gemini(&api_key, "gemini-2.5-flash");
        let client = sgr_agent::gemini::GeminiClient::new(config);

        let mut passed = 0;
        let total = prompts.len();

        for (name, prompt, expected_tool) in &prompts {
            print!("  {:<20} ", name);

            let messages = vec![
                sgr_agent::Message::system(SYSTEM),
                sgr_agent::Message::user(*prompt),
            ];

            match client.flexible::<NextStep>(&messages).await {
                Ok(resp) => {
                    if let Some(step) = resp.output {
                        let first = step.actions.first().map(tool_name).unwrap_or("(none)");
                        let ok = first == *expected_tool;
                        if ok { passed += 1; }

                        println!("{} tool={:<12} sit={:<30} acts={}",
                            if ok { "OK " } else { "WRONG" },
                            first,
                            step.situation.chars().take(30).collect::<String>(),
                            step.actions.len(),
                        );
                    } else {
                        println!("FAIL: empty output (raw: {})", &resp.raw_text[..resp.raw_text.len().min(60)]);
                    }
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    println!("ERR: {}", &msg[..msg.len().min(80)]);
                }
            }
        }

        println!("\n=== Results: {}/{} correct tool selection ===", passed, total);
    });
}
