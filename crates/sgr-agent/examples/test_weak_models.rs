//! Simulate weak model outputs and test flexible parser resilience.
//!
//! Real examples of what small/old models produce:
//! - Chain of thought before JSON
//! - Markdown wrapping
//! - Wrong field names
//! - YAML-ish output
//! - Hallucinated tools
//! - Partial/broken JSON
//! - Extra explanation after JSON
//!
//! Run: cargo run -p sgr-agent --example test_weak_models

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::flexible_parser::{parse_flexible, parse_flexible_coerced};

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
    #[serde(rename = "bash")]
    Bash { command: String },
    #[serde(rename = "finish")]
    Finish { summary: String },
    #[serde(rename = "edit_file")]
    EditFile {
        path: String,
        old_string: String,
        new_string: String,
    },
    #[serde(rename = "write_file")]
    WriteFile { path: String, content: String },
}

fn cases() -> Vec<(&'static str, &'static str)> {
    vec![
        // --- Weak model patterns ---
        (
            "1. Llama-style: thinks first, JSON after",
            "Let me analyze the request. The user wants me to read a file.\n\nI should use the read_file tool to read src/main.rs.\n\nHere is my response:\n\n{\"situation\": \"need to read file\", \"task\": [\"read src/main.rs\"], \"actions\": [{\"tool_name\": \"read_file\", \"path\": \"src/main.rs\"}]}",
        ),
        (
            "2. Phi-style: wraps in ```python block",
            "```python\n{\"situation\": \"checking tests\", \"task\": [\"run tests\"], \"actions\": [{\"tool_name\": \"bash\", \"command\": \"cargo test\"}]}\n```",
        ),
        (
            "3. Mistral-style: XML tags around JSON",
            "<response>\n{\"situation\": \"reading code\", \"task\": [\"read file\"], \"actions\": [{\"tool_name\": \"read_file\", \"path\": \"lib.rs\"}]}\n</response>",
        ),
        (
            "4. Gemma-style: repeats the schema first",
            "Based on the schema provided, I will respond with:\n- situation: a string describing the current state\n- task: an array of strings\n- actions: an array of tool calls\n\n{\"situation\": \"building project\", \"task\": [\"build\"], \"actions\": [{\"tool_name\": \"bash\", \"command\": \"cargo build\"}]}",
        ),
        (
            "5. Qwen-style: numbered reasoning then JSON",
            "1. First, I need to understand the situation\n2. The user wants to run tests\n3. I'll use the bash tool\n\nAnswer:\n{\"situation\": \"running tests\", \"task\": [\"execute test suite\"], \"actions\": [{\"tool_name\": \"bash\", \"command\": \"cargo test --all\"}]}",
        ),
        (
            "6. Wrong field: 'analysis' instead of 'situation'",
            "{\"analysis\": \"need to check code\", \"situation\": \"checking\", \"task\": [\"read\"], \"actions\": [{\"tool_name\": \"read_file\", \"path\": \"main.rs\"}]}",
        ),
        (
            "7. Extra fields: reasoning, confidence, etc",
            "{\"reasoning\": \"the user needs help\", \"confidence\": 0.95, \"situation\": \"helping user\", \"task\": [\"assist\"], \"actions\": [{\"tool_name\": \"finish\", \"summary\": \"done helping\"}], \"next_steps\": [\"monitor\"]}",
        ),
        (
            "8. Hallucinated tool name",
            "{\"situation\": \"searching\", \"task\": [\"find code\"], \"actions\": [{\"tool_name\": \"grep\", \"pattern\": \"TODO\", \"path\": \".\"}]}",
        ),
        (
            "9. Single quotes (Python-style JSON)",
            "{'situation': 'editing file', 'task': ['fix bug'], 'actions': [{'tool_name': 'edit_file', 'path': 'src/lib.rs', 'old_string': 'bug', 'new_string': 'fix'}]}",
        ),
        (
            "10. Trailing text after JSON",
            "{\"situation\": \"done\", \"task\": [\"complete\"], \"actions\": [{\"tool_name\": \"finish\", \"summary\": \"all done\"}]}\n\nI hope this helps! Let me know if you need anything else.",
        ),
        (
            "11. Double-wrapped JSON (model outputs JSON as string)",
            "\"{\\\"situation\\\": \\\"reading\\\", \\\"task\\\": [\\\"read\\\"], \\\"actions\\\": [{\\\"tool_name\\\": \\\"read_file\\\", \\\"path\\\": \\\"test.rs\\\"}]}\"",
        ),
        (
            "12. Markdown with extra backticks",
            "````json\n{\"situation\": \"writing\", \"task\": [\"create file\"], \"actions\": [{\"tool_name\": \"write_file\", \"path\": \"hello.rs\", \"content\": \"fn main() {}\"}]}\n````",
        ),
        (
            "13. Line-by-line JSON (pretty-printed with comments)",
            "{\n  // Current situation\n  \"situation\": \"analyzing code\",\n  // What needs to be done\n  \"task\": [\"review code\"],\n  // Actions to take\n  \"actions\": [\n    {\n      \"tool_name\": \"read_file\",\n      \"path\": \"src/main.rs\"\n    }\n  ]\n}",
        ),
        (
            "14. YAML output (Gemini 2.5 Flash sometimes does this)",
            "situation: need to check code\ntask:\n  - read main file\nactions:\n  - tool_name: read_file\n    path: src/main.rs",
        ),
        (
            "15. Empty actions (model refuses to act)",
            "{\"situation\": \"I'm not sure what to do\", \"task\": [\"think about it\"], \"actions\": []}",
        ),
        (
            "16. Actions as single object (not array)",
            "{\"situation\": \"reading\", \"task\": [\"read\"], \"actions\": {\"tool_name\": \"read_file\", \"path\": \"main.rs\"}}",
        ),
        (
            "17. Task as string (not array)",
            "{\"situation\": \"testing\", \"task\": \"run all tests\", \"actions\": [{\"tool_name\": \"bash\", \"command\": \"cargo test\"}]}",
        ),
        (
            "18. Truncated mid-key (streaming cutoff)",
            "{\"situation\": \"working on it\", \"task\": [\"fix the bug\"], \"actions\": [{\"tool_name\": \"edit_file\", \"path\": \"src/main.rs\", \"old_str",
        ),
        (
            "19. Unicode mess (Chinese model)",
            "{\"situation\": \"代码分析\", \"task\": [\"read file\"], \"actions\": [{\"tool_name\": \"read_file\", \"path\": \"src/main.rs\"}]}",
        ),
        (
            "20. Multiple JSON objects (model outputs alternatives)",
            "Option A:\n{\"situation\": \"wrong\", \"task\": [\"bad\"], \"actions\": [{\"tool_name\": \"bash\", \"command\": \"rm -rf /\"}]}\n\nOption B (recommended):\n{\"situation\": \"reading code\", \"task\": [\"review\"], \"actions\": [{\"tool_name\": \"read_file\", \"path\": \"src/main.rs\"}]}",
        ),
    ]
}

fn main() {
    println!("=== Weak Model Simulation: Flexible Parser Resilience ===\n");

    let cases = cases();
    let mut strict_pass = 0;
    let mut coerced_pass = 0;
    let total = cases.len();

    for (name, raw) in &cases {
        print!("  {:<52} ", name);

        let strict = parse_flexible::<NextStep>(raw);
        let coerced = parse_flexible_coerced::<NextStep>(raw);

        let s_ok = strict.is_ok();
        let c_ok = coerced.is_ok();
        if s_ok {
            strict_pass += 1;
        }
        if c_ok {
            coerced_pass += 1;
        }

        match (&strict, &coerced) {
            (Ok(s), _) => {
                let tool = s
                    .value
                    .actions
                    .first()
                    .map(|a| match a {
                        Action::ReadFile { .. } => "read_file",
                        Action::Bash { .. } => "bash",
                        Action::Finish { .. } => "finish",
                        Action::EditFile { .. } => "edit_file",
                        Action::WriteFile { .. } => "write_file",
                    })
                    .unwrap_or("(empty)");
                println!("strict:OK({:?}) tool={}", s.source, tool);
            }
            (Err(_), Ok(c)) => {
                let tool = c
                    .value
                    .actions
                    .first()
                    .map(|a| match a {
                        Action::ReadFile { .. } => "read_file",
                        Action::Bash { .. } => "bash",
                        Action::Finish { .. } => "finish",
                        Action::EditFile { .. } => "edit_file",
                        Action::WriteFile { .. } => "write_file",
                    })
                    .unwrap_or("(empty)");
                println!("strict:FAIL  coerced:OK({:?}) tool={}", c.source, tool);
            }
            (Err(se), Err(_)) => {
                println!("BOTH FAIL ({} candidates)", se.candidates.len());
            }
        }
    }

    println!("\n=== Results ===");
    println!("  strict:   {}/{}", strict_pass, total);
    println!("  coerced:  {}/{}", coerced_pass, total);

    let expected_hard = 3; // #8 hallucinated tool, #14 YAML, #18 truncated
    println!(
        "\n  Expected hard failures: {} (#8 unknown tool, #14 YAML, #18 truncated mid-key)",
        expected_hard
    );
    println!(
        "  Effective: {}/{} (strict), {}/{} (coerced)",
        strict_pass,
        total - expected_hard,
        coerced_pass,
        total - expected_hard,
    );
}
