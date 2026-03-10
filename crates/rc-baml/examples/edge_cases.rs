//! Edge case benchmark: BAML .parse() vs sgr-agent flexible parser.
//!
//! Tests both parsers on identical raw LLM output strings — from clean
//! JSON to maximally broken/messy responses.
//!
//! Run: cargo run -p rc-baml --example edge_cases

use rc_baml::baml_client::B;

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
            "18. Markdown with language tag and indent",
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
    println!("=== Edge Case Benchmark: BAML .parse() vs sgr-agent ===\n");

    let cases = edge_cases();
    let mut baml_pass = 0;
    let mut baml_total = 0;

    for (name, raw) in &cases {
        baml_total += 1;
        print!("  {:<45} BAML: ", name);

        match B.GetNextStep.parse(raw) {
            Ok(step) => {
                baml_pass += 1;
                let acts: Vec<String> = step.actions.iter().map(|a| format!("{:?}", a).chars().take(40).collect::<String>()).collect();
                println!("OK  sit={:<25} acts={}",
                    step.situation.chars().take(25).collect::<String>(),
                    acts.len()
                );
            }
            Err(e) => {
                let msg = format!("{}", e);
                let short = if msg.len() > 60 { &msg[..60] } else { &msg };
                println!("FAIL  {}", short);
            }
        }
    }

    println!("\n=== Results ===");
    println!("  BAML:       {}/{}", baml_pass, baml_total);
    println!("\nRun the sgr-agent side with:");
    println!("  cargo run -p sgr-agent --example edge_cases_sgr");
}
