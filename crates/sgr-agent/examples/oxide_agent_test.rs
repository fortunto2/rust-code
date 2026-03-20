//! Integration test: real agent loop through Llm (oxide backend) with GPT-5.4.
//!
//! Tests: backend selection, structured output, function calling, multi-turn.
//!
//! ```bash
//! OPENAI_API_KEY=sk-... cargo run -p sgr-agent --example oxide_agent_test --features "oxide,genai"
//! ```

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::Llm;
use sgr_agent::client::LlmClient;
use sgr_agent::tool::ToolDef;
use sgr_agent::types::{LlmConfig, Message};
use std::time::Instant;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct CodeReview {
    file: String,
    issues: Vec<Issue>,
    overall_score: u8,
    suggestion: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
struct Issue {
    line: u32,
    severity: String,
    message: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = LlmConfig::auto("gpt-5.4").temperature(0.3).max_tokens(800);
    let llm = Llm::new(&config);

    println!("Backend: {}\n", llm.backend_name());
    assert_eq!(
        llm.backend_name(),
        "oxide",
        "Expected oxide backend for gpt-5.4"
    );

    let t_total = Instant::now();

    // ── Test 1: Plain completion ──
    print!("1. Plain completion... ");
    let t0 = Instant::now();
    let result = llm
        .generate(&[
            Message::system("You are a senior Rust developer. Be concise."),
            Message::user(
                "What's the difference between Box<dyn Trait> and impl Trait? One paragraph.",
            ),
        ])
        .await?;
    println!(
        "OK ({}ms) — {} chars",
        t0.elapsed().as_millis(),
        result.len()
    );
    assert!(result.contains("Box") || result.contains("dynamic"));

    // ── Test 2: Structured output ──
    print!("2. Structured output... ");
    let t0 = Instant::now();
    let review: CodeReview = llm
        .structured(&[
            Message::system(
                "You are a code reviewer. Analyze the code and return structured feedback.",
            ),
            Message::user(
                r#"Review this code:
```rust
fn process(data: &Vec<String>) -> Vec<String> {
    let mut result = vec![];
    for i in 0..data.len() {
        if data[i].len() > 0 {
            result.push(data[i].clone().to_uppercase());
        }
    }
    return result;
}
```"#,
            ),
        ])
        .await?;
    println!(
        "OK ({}ms) — {} issues, score {}/10",
        t0.elapsed().as_millis(),
        review.issues.len(),
        review.overall_score
    );
    assert!(!review.issues.is_empty(), "Should find issues in the code");
    assert!(review.overall_score <= 10);
    for issue in &review.issues {
        println!("   L{}: [{}] {}", issue.line, issue.severity, issue.message);
    }

    // ── Test 3: Function calling ──
    print!("3. Function calling... ");
    let tools = vec![
        ToolDef {
            name: "read_file".into(),
            description: "Read a file from the project".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path relative to project root"}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        },
        ToolDef {
            name: "run_tests".into(),
            description: "Run tests for a module".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "module": {"type": "string", "description": "Module name to test"}
                },
                "required": ["module"],
                "additionalProperties": false
            }),
        },
    ];
    let t0 = Instant::now();
    let calls = llm.tools_call(&[
        Message::system("You are a coding agent. Use tools to investigate before answering."),
        Message::user("Check the auth module for security issues. Read the file first, then run its tests."),
    ], &tools).await?;
    println!(
        "OK ({}ms) — {} tool call(s)",
        t0.elapsed().as_millis(),
        calls.len()
    );
    assert!(!calls.is_empty(), "Should make at least one tool call");
    for tc in &calls {
        println!("   {}({})", tc.name, tc.arguments);
    }

    // ── Test 4: FC → provide result → structured response (2-step agent loop) ──
    print!("4. Agent loop... ");
    let t0 = Instant::now();

    // Step 1: FC to decide what file to read
    let llm2 = Llm::new(&config);
    let step1_calls = llm2
        .tools_call(
            &[
                Message::system("You are a code review agent. Read the file first."),
                Message::user("Review src/auth.rs for security issues"),
            ],
            &[tools[0].clone()],
        )
        .await?;
    assert!(!step1_calls.is_empty(), "Step 1 should produce a tool call");
    println!(
        "step1: {}({}) ",
        step1_calls[0].name, step1_calls[0].arguments
    );

    // Step 2: Fresh Llm (no previous_response_id) — structured review with code context
    let llm3 = Llm::new(&config);
    let schema = sgr_agent::response_schema_for::<CodeReview>();
    let (parsed, _, _) = llm3
        .structured_call(
            &[
                Message::system(
                    "You are a code review agent. Analyze the code and return structured review.",
                ),
                Message::user(
                    r#"I read src/auth.rs. Here's the content:

```rust
fn authenticate(token: &str) -> bool {
    if token == "admin123" { return true; }  // hardcoded credential!
    let hash = sha256(token);
    DB.verify(hash)
}
```

Review it for security issues."#,
                ),
            ],
            &schema,
        )
        .await?;
    let review: CodeReview = serde_json::from_value(parsed.unwrap())?;
    println!(
        "OK ({}ms) — {} issues found",
        t0.elapsed().as_millis(),
        review.issues.len()
    );
    assert!(
        review
            .issues
            .iter()
            .any(|i| i.message.to_lowercase().contains("hardcod")
                || i.message.to_lowercase().contains("credential")
                || i.message.to_lowercase().contains("password")),
        "Should detect hardcoded credential"
    );

    let total = t_total.elapsed().as_millis();
    println!(
        "\n=== All 4 tests passed ({total}ms total, backend: {}) ===",
        llm.backend_name()
    );

    Ok(())
}
