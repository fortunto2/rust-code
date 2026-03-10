//! Edge case benchmark: sgr-agent flexible parser.
//!
//! Same test cases as rc-baml/examples/edge_cases.rs.
//! Matching schema to BAML's NextStep (18-tool union).
//!
//! Run: cargo run -p sgr-agent --example edge_cases_sgr

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::flexible_parser::{parse_flexible, parse_flexible_coerced};

// Schema matching BAML's NextStep as closely as possible.
// BAML union is by field presence; sgr-agent uses serde tagged enum.

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
struct NextStep {
    situation: String,
    task: TaskField,
    actions: Vec<Action>,
}

// task can be string or string[] — handle both
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
enum TaskField {
    Array(Vec<String>),
    Single(String),
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
        #[serde(default)]
        timeout: Option<i64>,
    },
    #[serde(rename = "bash_bg")]
    BashBg { name: String, command: String },
    #[serde(rename = "search_code")]
    SearchCode { query: String },
    #[serde(rename = "git_status")]
    GitStatus {
        #[serde(default)]
        dummy: Option<String>,
    },
    #[serde(rename = "git_diff")]
    GitDiff {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        cached: Option<bool>,
    },
    #[serde(rename = "git_add")]
    GitAdd { paths: Vec<String> },
    #[serde(rename = "git_commit")]
    GitCommit { message: String },
    #[serde(rename = "open_editor")]
    OpenEditor {
        path: String,
        #[serde(default)]
        line: Option<i64>,
    },
    #[serde(rename = "ask_user")]
    AskUser { question: String },
    #[serde(rename = "finish")]
    Finish { summary: String },
    #[serde(rename = "mcp_call")]
    McpCall {
        server: String,
        tool: String,
        #[serde(default)]
        arguments: Option<String>,
    },
    #[serde(rename = "memory")]
    Memory {
        operation: String,
        #[serde(default)]
        content: Option<String>,
    },
    #[serde(rename = "project_map")]
    ProjectMap {
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "dependencies")]
    Dependencies {
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "task")]
    Task {
        operation: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        task_id: Option<i64>,
    },
}

fn edge_cases() -> Vec<(&'static str, &'static str)> {
    vec![
        // === CLEAN ===
        (
            "1. Clean JSON",
            r#"{"situation":"reading file","task":["read src/main.rs"],"actions":[{"tool_name":"read_file","path":"src/main.rs"}]}"#,
        ),

        // === MARKDOWN WRAPPING ===
        (
            "2. Markdown ```json block",
            "```json\n{\"situation\":\"checking deps\",\"task\":[\"read Cargo.toml\"],\"actions\":[{\"tool_name\":\"read_file\",\"path\":\"Cargo.toml\"}]}\n```",
        ),
        (
            "3. Markdown ``` no lang tag",
            "```\n{\"situation\":\"building\",\"task\":[\"run cargo build\"],\"actions\":[{\"tool_name\":\"bash\",\"command\":\"cargo build\"}]}\n```",
        ),

        // === CHAIN OF THOUGHT ===
        (
            "4. CoT then JSON",
            "Let me think about this step by step.\n\n1. First I need to read the file\n2. Then fix the error\n\n{\"situation\":\"file has error\",\"task\":[\"read and fix\"],\"actions\":[{\"tool_name\":\"read_file\",\"path\":\"src/lib.rs\"}]}",
        ),
        (
            "5. CoT + markdown block",
            "I'll analyze the situation:\n- The project needs a new feature\n- Let me start by reading the code\n\n```json\n{\"situation\":\"adding feature\",\"task\":[\"read code first\"],\"actions\":[{\"tool_name\":\"read_file\",\"path\":\"src/main.rs\"}]}\n```\n\nThis should give us what we need.",
        ),

        // === BROKEN JSON ===
        (
            "6. Trailing comma in actions array",
            r#"{"situation":"creating file","task":["write hello.rs"],"actions":[{"tool_name":"write_file","path":"hello.rs","content":"fn main() {}"},]}"#,
        ),
        (
            "7. Trailing comma in object",
            r#"{"situation":"done","task":["finish"],"actions":[{"tool_name":"finish","summary":"all done",},],}"#,
        ),
        (
            "8. JS-style comments",
            "{\n  // Assess the situation\n  \"situation\": \"need to check\",\n  \"task\": [\"run tests\"],\n  \"actions\": [\n    // Run the test suite\n    {\"tool_name\": \"bash\", \"command\": \"cargo test\"}\n  ]\n}",
        ),
        (
            "9. Single quotes",
            "{'situation': 'checking code', 'task': ['read file'], 'actions': [{'tool_name': 'read_file', 'path': 'main.rs'}]}",
        ),

        // === TRUNCATED ===
        (
            "10. Missing closing brackets",
            r#"{"situation":"in progress","task":["continue work"],"actions":[{"tool_name":"bash","command":"cargo build"}]"#,
        ),

        // === MULTI-ACTION (complex real-world) ===
        (
            "11. Multiple actions",
            r#"{"situation":"need to read multiple files","task":["read 3 files","analyze deps"],"actions":[{"tool_name":"read_file","path":"src/main.rs"},{"tool_name":"read_file","path":"src/lib.rs"},{"tool_name":"read_file","path":"Cargo.toml"}]}"#,
        ),

        // === ESCAPED CONTENT ===
        (
            "12. Content with escaped quotes and newlines",
            r#"{"situation":"writing code","task":["create file"],"actions":[{"tool_name":"write_file","path":"test.rs","content":"fn main() {\n    println!(\"hello \\\"world\\\"\");\n}"}]}"#,
        ),

        // === WRONG FIELD ORDER ===
        (
            "13. Actions first, situation last",
            r#"{"actions":[{"tool_name":"finish","summary":"done"}],"task":["wrap up"],"situation":"completed all work"}"#,
        ),

        // === NESTED JSON IN CONTENT ===
        (
            "14. JSON inside string field",
            r#"{"situation":"writing config","task":["create config file"],"actions":[{"tool_name":"write_file","path":"config.json","content":"{\"key\": \"value\", \"port\": 8080}"}]}"#,
        ),

        // === UNICODE ===
        (
            "15. Cyrillic content",
            r#"{"situation":"writing Russian text","task":["create file"],"actions":[{"tool_name":"write_file","path":"hello.txt","content":"Привет мир! Это тест."}]}"#,
        ),

        // === EDIT TOOL (tricky: old_string/new_string with code) ===
        (
            "16. Edit tool with code in strings",
            r#"{"situation":"fixing function","task":["edit file"],"actions":[{"tool_name":"edit_file","path":"src/lib.rs","old_string":"fn foo() {","new_string":"fn foo() -> i32 {"}]}"#,
        ),

        // === EXTRA FIELDS (model adds fields not in schema) ===
        (
            "17. Extra unknown fields",
            r#"{"situation":"analyzing","task":["read file"],"reasoning":"I should check the file first","confidence":0.95,"actions":[{"tool_name":"read_file","path":"main.rs"}]}"#,
        ),

        // === DEEPLY NESTED MARKDOWN ===
        (
            "18. Markdown with indent",
            "Here is my response:\n\n    ```json\n    {\n      \"situation\": \"starting\",\n      \"task\": [\"begin work\"],\n      \"actions\": [{\"tool_name\": \"bash\", \"command\": \"ls\"}]\n    }\n    ```",
        ),

        // === TASK AS STRING (not array) ===
        (
            "19. task as string instead of string[]",
            r#"{"situation":"reading","task":"read the main file","actions":[{"tool_name":"read_file","path":"main.rs"}]}"#,
        ),

        // === EMPTY ACTIONS ===
        (
            "20. Empty actions array",
            r#"{"situation":"thinking","task":["decide what to do"],"actions":[]}"#,
        ),

        // === MCP TOOL (complex arguments field) ===
        (
            "21. McpToolCall with JSON arguments string",
            r#"{"situation":"searching code","task":["search for auth"],"actions":[{"tool_name":"mcp_call","server":"codegraph","tool":"project_code_search","arguments":"{\"query\": \"auth middleware\", \"project\": \"myapp\"}"}]}"#,
        ),

        // === MULTIPLE TOOL TYPES IN ONE STEP ===
        (
            "22. Mixed tool types",
            r##"{"situation":"setting up","task":["read, create, and run"],"actions":[{"tool_name":"read_file","path":"Cargo.toml"},{"tool_name":"write_file","path":"test.rs","content":"#[test]\nfn it_works() {}"},{"tool_name":"bash","command":"cargo test"}]}"##,
        ),

        // === STREAMING PARTIAL (incomplete mid-action) ===
        (
            "23. Truncated mid-action (streaming)",
            r#"{"situation":"working","task":["fix bug"],"actions":[{"tool_name":"edit_file","path":"src/main.rs","old_string":"let x = 1","new_stri"#,
        ),

        // === COMPLETELY WRONG FORMAT ===
        (
            "24. Plain text (no JSON at all)",
            "I'll read the file src/main.rs and check for any issues with the return type. Let me do that now.",
        ),

        // === YAML-LIKE (Gemini 2.5 Flash sometimes does this) ===
        (
            "25. YAML-ish output",
            "situation: checking the code\ntask:\n  - read main file\nactions:\n  - tool_name: read_file\n    path: src/main.rs",
        ),
    ]
}

fn main() {
    println!("=== Edge Case Benchmark: sgr-agent flexible parser ===\n");

    let cases = edge_cases();
    let mut strict_pass = 0;
    let mut coerced_pass = 0;
    let total = cases.len();

    for (name, raw) in &cases {
        print!("  {:<45} ", name);

        let strict = parse_flexible::<NextStep>(raw);
        let coerced = parse_flexible_coerced::<NextStep>(raw);

        match (&strict, &coerced) {
            (Ok(s), Ok(c)) => {
                strict_pass += 1;
                coerced_pass += 1;
                println!(
                    "strict:OK({:?},{})  coerced:OK({})",
                    s.source, s.candidates_tried, c.candidates_tried
                );
            }
            (Ok(s), Err(_)) => {
                strict_pass += 1;
                println!("strict:OK({:?},{})  coerced:FAIL", s.source, s.candidates_tried);
            }
            (Err(_), Ok(c)) => {
                coerced_pass += 1;
                println!(
                    "strict:FAIL              coerced:OK({:?},{})",
                    c.source, c.candidates_tried
                );
            }
            (Err(se), Err(_)) => {
                println!("strict:FAIL({} cands)     coerced:FAIL", se.candidates.len());
            }
        }
    }

    println!("\n=== Results ===");
    println!("  parse_flexible (strict):  {}/{}", strict_pass, total);
    println!("  parse_flexible_coerced:   {}/{}", coerced_pass, total);

    // Expected failures: #23 (truncated mid-action), #24 (plain text), #25 (YAML)
    let expected_fails = 3;
    println!("\n  Expected hard failures: {} (#23 truncated mid-field, #24 no JSON, #25 YAML)", expected_fails);
    println!(
        "  Effective: {}/{} (strict), {}/{} (coerced)",
        strict_pass,
        total - expected_fails,
        coerced_pass,
        total - expected_fails
    );
}
