//! Test multiple Gemini models via SGR flexible parser.
//!
//! Compares tool selection accuracy across models.
//!
//! Run: cargo run -p sgr-agent --example test_models

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
    #[serde(rename = "search_code")]
    SearchCode { query: String },
    #[serde(rename = "finish")]
    Finish { summary: String },
    #[serde(rename = "ask_user")]
    AskUser { question: String },
    #[serde(rename = "git_status")]
    GitStatus { #[serde(default)] dummy: Option<String> },
    #[serde(rename = "git_diff")]
    GitDiff { #[serde(default)] path: Option<String> },
    #[serde(rename = "git_add")]
    GitAdd { paths: Vec<String> },
    #[serde(rename = "git_commit")]
    GitCommit { message: String },
    #[serde(rename = "mcp_call")]
    McpCall { server: String, tool: String, #[serde(default)] arguments: Option<String> },
    #[serde(rename = "memory")]
    Memory { operation: String, #[serde(default)] content: Option<String> },
    #[serde(rename = "project_map")]
    ProjectMap { #[serde(default)] path: Option<String> },
}

const SYSTEM: &str = "You are an AI coding agent. Available tools: read_file, write_file, edit_file, bash, search_code, finish, ask_user, git_status, git_diff, git_add, git_commit, mcp_call, memory, project_map.\n\
    Respond with JSON: {\"situation\": \"...\", \"task\": [\"...\"], \"actions\": [{\"tool_name\": \"...\", ...}]}";

fn tool_name(action: &Action) -> &str {
    match action {
        Action::ReadFile { .. } => "read_file",
        Action::WriteFile { .. } => "write_file",
        Action::EditFile { .. } => "edit_file",
        Action::Bash { .. } => "bash",
        Action::SearchCode { .. } => "search_code",
        Action::Finish { .. } => "finish",
        Action::AskUser { .. } => "ask_user",
        Action::GitStatus { .. } => "git_status",
        Action::GitDiff { .. } => "git_diff",
        Action::GitAdd { .. } => "git_add",
        Action::GitCommit { .. } => "git_commit",
        Action::McpCall { .. } => "mcp_call",
        Action::Memory { .. } => "memory",
        Action::ProjectMap { .. } => "project_map",
    }
}

struct TestCase {
    name: &'static str,
    prompt: &'static str,
    /// Any of these tools is acceptable as first action
    accept: &'static [&'static str],
}

fn test_cases() -> Vec<TestCase> {
    vec![
        TestCase {
            name: "1. Read file",
            prompt: "Read src/main.rs",
            accept: &["read_file"],
        },
        TestCase {
            name: "2. Run tests",
            prompt: "Run the test suite with cargo test",
            accept: &["bash"],
        },
        TestCase {
            name: "3. Git status",
            prompt: "Show me the current git status",
            accept: &["git_status", "bash"],
        },
        TestCase {
            name: "4. Commit",
            prompt: "Stage all changes and commit with message 'fix: typo in readme'",
            accept: &["git_add", "bash"],
        },
        TestCase {
            name: "5. Multi-step",
            prompt: "Read Cargo.toml then run cargo build",
            accept: &["read_file"],
        },
        TestCase {
            name: "6. Finish",
            prompt: "Done. Summary: all tests pass, code reviewed.",
            accept: &["finish"],
        },
        TestCase {
            name: "7. Remember",
            prompt: "Remember that this project uses PostgreSQL 16",
            accept: &["memory"],
        },
        TestCase {
            name: "8. Project map",
            prompt: "Show me the project structure",
            accept: &["project_map", "bash"],
        },
    ]
}

#[tokio::main]
async fn main() {
    let api_key = std::env::var("GEMINI_API_KEY").expect("GEMINI_API_KEY required");

    // Weakest → strongest (2.0 models removed — 404 from API)
    let models = vec![
        "gemini-2.5-flash-lite",         // new gen lite
        "gemini-2.5-flash",              // new gen main
        "gemini-3-flash-preview",        // next gen preview
        "gemini-3.1-flash-lite-preview", // latest lite
    ];

    let cases = test_cases();

    for model in &models {
        println!("━━━ {} ━━━", model);

        let config = sgr_agent::ProviderConfig::gemini(&api_key, *model);
        let client = sgr_agent::gemini::GeminiClient::new(config);

        let mut passed = 0;
        let total = cases.len();

        for case in &cases {
            print!("  {:<20} ", case.name);

            let messages = vec![
                sgr_agent::Message::system(SYSTEM),
                sgr_agent::Message::user(case.prompt),
            ];

            match client.flexible::<NextStep>(&messages).await {
                Ok(resp) => {
                    if let Some(step) = resp.output {
                        let first = step.actions.first().map(tool_name).unwrap_or("(none)");
                        let ok = case.accept.contains(&first);
                        if ok { passed += 1; }

                        println!("{} tool={:<14} acts={} sit={}",
                            if ok { "OK   " } else { "WRONG" },
                            first,
                            step.actions.len(),
                            step.situation.chars().take(40).collect::<String>(),
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

        println!("  Score: {}/{}\n", passed, total);
    }
}
