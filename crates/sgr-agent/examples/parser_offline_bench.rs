//! Offline benchmark: flexible parser on realistic LLM responses.
//!
//! Tests parsing without any API calls — uses hardcoded responses
//! that simulate real Codex/GPT/Claude output patterns.
//!
//! Run: cargo run -p sgr-agent --example parser_offline_bench

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::flexible_parser::{parse_flexible, parse_flexible_coerced};

// --- Same schema as compare_parsers ---

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

// --- Simulated LLM responses ---

fn test_responses() -> Vec<(&'static str, &'static str)> {
    vec![
        // 1. Clean JSON (ideal case)
        (
            "Clean JSON",
            r#"{"situation":"Need to read the main file","task":"Read src/main.rs to understand the project","actions":[{"tool_name":"read_file","path":"src/main.rs"}]}"#,
        ),
        // 2. JSON in markdown code block
        (
            "Markdown ```json block",
            r#"Here's my plan:

```json
{
  "situation": "The user wants to check dependencies",
  "task": "Read and analyze Cargo.toml",
  "actions": [
    {"tool_name": "read_file", "path": "Cargo.toml"}
  ]
}
```

This will help us understand the project structure."#,
        ),
        // 3. Markdown ``` block (no language tag)
        (
            "Markdown ``` block (no lang)",
            r#"I'll read the file first.

```
{
  "situation": "Checking compilation errors",
  "task": "Read lib.rs and fix return type",
  "actions": [
    {"tool_name": "read_file", "path": "src/lib.rs"},
    {"tool_name": "write_file", "path": "src/lib.rs", "content": "fn foo() -> i32 { 42 }"}
  ]
}
```"#,
        ),
        // 4. Chain-of-thought then JSON
        (
            "CoT before JSON",
            r#"Let me think about this step by step:

1. First, I need to understand the current state of the code
2. The function foo() is missing a return type
3. I should read the file, then fix it

{
  "situation": "Function foo() has no return type annotation",
  "task": "Add return type to foo()",
  "actions": [
    {"tool_name": "read_file", "path": "src/lib.rs"},
    {"tool_name": "write_file", "path": "src/lib.rs", "content": "pub fn foo() -> String {\n    \"hello\".to_string()\n}"}
  ]
}"#,
        ),
        // 5. JSON with trailing commas (common LLM mistake)
        (
            "Trailing commas",
            r#"{
  "situation": "Creating hello world",
  "task": "Write hello.rs",
  "actions": [
    {"tool_name": "write_file", "path": "hello.rs", "content": "fn main() {\n    println!(\"Hello!\");\n}"},
  ],
}"#,
        ),
        // 6. JSON with comments (another common mistake)
        (
            "JSON with comments",
            r#"{
  // Current state
  "situation": "Project needs a new file",
  "task": "Create the requested file",
  "actions": [
    {"tool_name": "write_file", "path": "hello.rs", "content": "fn main() { println!(\"Hi\"); }"}
  ]
}"#,
        ),
        // 7. Single-quoted JSON
        (
            "Single quotes",
            r#"{'situation': 'Reading the project', 'task': 'Check main.rs', 'actions': [{'tool_name': 'read_file', 'path': 'src/main.rs'}]}"#,
        ),
        // 8. Text wrapping with explanation after
        (
            "JSON sandwiched in text",
            r#"Based on my analysis, here is the action plan:

{"situation":"Need to run tests","task":"Execute cargo test","actions":[{"tool_name":"bash","command":"cargo test"}]}

I'll execute the tests and report back with the results."#,
        ),
        // 9. Multiple JSON objects (should pick the right one)
        (
            "Multiple JSON objects",
            r#"Here are two options:

Option A (simpler):
{"situation":"Quick check","task":"Just read","actions":[{"tool_name":"read_file","path":"README.md"}]}

Option B (thorough):
{"situation":"Full analysis","task":"Read and test","actions":[{"tool_name":"read_file","path":"src/main.rs"},{"tool_name":"bash","command":"cargo test"}]}

I recommend Option B."#,
        ),
        // 10. Truncated JSON (unclosed brackets)
        (
            "Truncated JSON (unclosed)",
            r#"{"situation":"Working on the fix","task":"Apply patch","actions":[{"tool_name":"write_file","path":"fix.rs","content":"fn fixed() {}"}]"#,
        ),
        // 11. Type coercion needed: number as string
        (
            "Type coercion: no actions array",
            r#"{"situation":"Done with task","task":"Finished","actions":[{"tool_name":"finish","summary":"All done"}]}"#,
        ),
        // 12. Unicode in content
        (
            "Unicode content",
            r#"{"situation":"Создание файла","task":"Написать hello.rs","actions":[{"tool_name":"write_file","path":"hello.rs","content":"fn main() {\n    println!(\"Привет мир!\");\n}"}]}"#,
        ),
        // 13. Deeply nested markdown
        (
            "Nested markdown blocks",
            r#"## Plan

Here's what I'll do:

```json
{
  "situation": "Need to build the project",
  "task": "Run cargo build",
  "actions": [
    {
      "tool_name": "bash",
      "command": "cargo build 2>&1"
    }
  ]
}
```

### Next steps
After building, I'll run tests."#,
        ),
        // 14. JSON with extra whitespace/newlines
        (
            "Extra whitespace",
            r#"

  {
    "situation"  :  "Checking the code"  ,
    "task"  :  "Read and analyze"  ,
    "actions"  :  [
      {  "tool_name"  :  "read_file"  ,  "path"  :  "main.rs"  }
    ]
  }

"#,
        ),
        // 15. Response that's just the text (should fail gracefully)
        (
            "Plain text (no JSON)",
            "I'll read the file src/main.rs and check for any issues with the return type.",
        ),
    ]
}

fn main() {
    println!("╔══════════════════════════════════════════════════════╗");
    println!("║  Offline parser benchmark — 15 realistic patterns   ║");
    println!("╚══════════════════════════════════════════════════════╝\n");

    let cases = test_responses();
    let mut pass_strict = 0;
    let mut pass_coerced = 0;
    let total = cases.len();

    for (name, raw) in &cases {
        print!("  {:<35} ", name);

        let strict = parse_flexible::<NextStep>(raw);
        let coerced = parse_flexible_coerced::<NextStep>(raw);

        match (&strict, &coerced) {
            (Ok(s), Ok(c)) => {
                pass_strict += 1;
                pass_coerced += 1;
                println!(
                    "strict ✓ ({:?}, tried {})  coerced ✓ (tried {})",
                    s.source, s.candidates_tried, c.candidates_tried
                );
            }
            (Ok(s), Err(_)) => {
                pass_strict += 1;
                println!(
                    "strict ✓ ({:?}, tried {})  coerced ✗",
                    s.source, s.candidates_tried
                );
            }
            (Err(_), Ok(c)) => {
                pass_coerced += 1;
                println!(
                    "strict ✗                   coerced ✓ ({:?}, tried {})",
                    c.source, c.candidates_tried
                );
            }
            (Err(se), Err(_ce)) => {
                println!("strict ✗ ({} cands)        coerced ✗", se.candidates.len());
            }
        }
    }

    println!("\n═══ Results ═══");
    println!("  parse_flexible (strict):  {}/{}", pass_strict, total);
    println!("  parse_flexible_coerced:   {}/{}", pass_coerced, total);
    let expected_fails = 1; // #15 is plain text, should fail
    println!(
        "\n  Expected failures: {} (plain text without JSON)",
        expected_fails
    );
    println!(
        "  Effective score: {}/{} (strict), {}/{} (coerced)",
        pass_strict,
        total - expected_fails,
        pass_coerced,
        total - expected_fails
    );
}
