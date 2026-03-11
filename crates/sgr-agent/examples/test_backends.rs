//! Integration test: SGR flexible parser on real Gemini responses.
//!
//! Tests the full pipeline: prompt → Gemini API → flexible parse → typed NextStep.
//! This is what the `--sgr` backend does inside rc-cli.
//!
//! Run: cargo run -p sgr-agent --example test_backends

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// Mirror of rc-cli's backend::SgrNextStep — same schema the agent uses
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
    ReadFile {
        path: String,
        #[serde(default)]
        offset: Option<i64>,
        #[serde(default)]
        limit: Option<i64>,
    },
    #[serde(rename = "write_file")]
    WriteFile { path: String, content: String },
    #[serde(rename = "edit_file")]
    EditFile {
        path: String,
        old_string: String,
        new_string: String,
    },
    #[serde(rename = "bash")]
    Bash {
        command: String,
        #[serde(default)]
        description: Option<String>,
    },
    #[serde(rename = "search_code")]
    SearchCode { query: String },
    #[serde(rename = "finish")]
    Finish { summary: String },
    #[serde(rename = "ask_user")]
    AskUser { question: String },
}

const SYSTEM: &str = "You are an AI coding agent. Available tools: read_file, write_file, edit_file, bash, search_code, finish, ask_user.\n\
    Always respond with JSON: {\"situation\": \"...\", \"task\": [\"...\"], \"actions\": [{\"tool_name\": \"...\", ...}]}";

#[tokio::main]
async fn main() {
    println!("=== SGR Backend Integration Test ===\n");

    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY required");

    let prompts: Vec<(&str, &str, &str)> = vec![
        (
            "1. Read file",
            "Read the file src/main.rs to understand the project structure.",
            "read_file",
        ),
        (
            "2. Run command",
            "Run `cargo test` to check if tests pass.",
            "bash",
        ),
        (
            "3. Edit file",
            "In src/lib.rs, change the function name from `old_name` to `new_name`.",
            "edit_file",
        ),
        (
            "4. Multi-action",
            "First read Cargo.toml, then run `cargo build`.",
            "read_file",
        ),
        (
            "5. Finish",
            "All tasks are complete. Summarize: fixed 3 bugs, added 2 tests.",
            "finish",
        ),
        (
            "6. Search",
            "Find all uses of `parse_flexible` in the codebase.",
            "search_code",
        ),
        (
            "7. Ask user",
            "I need clarification about which database to use.",
            "ask_user",
        ),
    ];

    let config = sgr_agent::ProviderConfig::gemini(&api_key, "gemini-2.5-flash");
    let client = sgr_agent::gemini::GeminiClient::new(config);

    let mut passed = 0;
    let mut total = 0;

    for (name, prompt, expected_tool) in &prompts {
        total += 1;
        print!("  {:<25} ", name);

        let messages = vec![
            sgr_agent::Message::system(SYSTEM),
            sgr_agent::Message::user(*prompt),
        ];

        match client.flexible::<NextStep>(&messages).await {
            Ok(resp) => {
                if let Some(step) = resp.output {
                    let first_tool = step
                        .actions
                        .first()
                        .map(|a| match a {
                            Action::ReadFile { .. } => "read_file",
                            Action::WriteFile { .. } => "write_file",
                            Action::EditFile { .. } => "edit_file",
                            Action::Bash { .. } => "bash",
                            Action::SearchCode { .. } => "search_code",
                            Action::Finish { .. } => "finish",
                            Action::AskUser { .. } => "ask_user",
                        })
                        .unwrap_or("(none)");

                    let tool_ok = first_tool == *expected_tool;
                    if tool_ok {
                        passed += 1;
                    }

                    println!(
                        "{} tool={:<12} sit={:<35} acts={}",
                        if tool_ok { "OK " } else { "WRONG" },
                        first_tool,
                        step.situation.chars().take(35).collect::<String>(),
                        step.actions.len(),
                    );
                } else {
                    println!("FAIL: empty output");
                }
            }
            Err(e) => {
                let msg = format!("{}", e);
                println!("ERR: {}", &msg[..msg.len().min(80)]);
            }
        }
    }

    println!(
        "\n=== Results: {}/{} correct tool selection ===",
        passed, total
    );
}
