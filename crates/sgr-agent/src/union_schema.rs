//! Dynamic discriminated union schema builder — generates oneOf JSON Schema
//! from tool definitions at runtime. Used by SgrAgent for structured output.

use crate::tool::ToolDef;
use crate::types::ToolCall;
use serde_json::Value;

/// Build a JSON Schema `oneOf` from tool definitions.
/// Each variant has a `tool_name` const discriminator.
pub fn build_action_schema(tools: &[ToolDef]) -> Value {
    let variants: Vec<Value> = tools
        .iter()
        .map(|t| {
            let mut properties = serde_json::Map::new();

            // Discriminator
            properties.insert(
                "tool_name".to_string(),
                serde_json::json!({ "type": "string", "const": t.name }),
            );

            // Merge tool parameters into properties
            if let Some(props) = t.parameters.get("properties").and_then(|p| p.as_object()) {
                for (k, v) in props {
                    properties.insert(k.clone(), v.clone());
                }
            }

            // Required fields: tool_name + tool's required
            let mut required = vec![serde_json::json!("tool_name")];
            if let Some(req) = t.parameters.get("required").and_then(|r| r.as_array()) {
                required.extend(req.iter().cloned());
            }

            serde_json::json!({
                "type": "object",
                "properties": properties,
                "required": required,
            })
        })
        .collect();

    serde_json::json!({
        "type": "object",
        "properties": {
            "situation": { "type": "string", "description": "Current assessment" },
            "task": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Reasoning steps"
            },
            "actions": {
                "type": "array",
                "items": { "oneOf": variants },
                "description": "Tool calls to execute"
            }
        },
        "required": ["situation", "task", "actions"]
    })
}

/// Known wrapper keys that Gemini uses to wrap tool arguments.
const WRAPPER_KEYS: &[&str] = &["parameters", "params", "args", "arguments"];

/// Parse raw LLM output into tool calls using flexible_parser.
/// Extracts `actions` array and maps each to a ToolCall.
pub fn parse_action(raw: &str, _tools: &[ToolDef]) -> Result<(String, Vec<ToolCall>), ParseError> {
    // Try to parse as JSON via flexible parser, fall back to direct serde
    let value: Value = match crate::flexible_parser::parse_flexible::<Value>(raw) {
        Ok(r) => r.value,
        Err(_) => serde_json::from_str::<Value>(raw)
            .map_err(|e| ParseError(e.to_string()))?,
    };

    let situation = match value.get("situation") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };

    let actions: Vec<Value> = match value.get("actions") {
        Some(Value::Array(arr)) => arr.clone(),
        _ => Vec::new(),
    };

    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for (i, action) in actions.into_iter().enumerate() {
        let name = match action.get("tool_name") {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };

        // Remove tool_name from args, unwrap "parameters" wrapper if present
        let arguments = if let Value::Object(mut obj) = action {
            obj.remove("tool_name");
            // Gemini sometimes wraps args: {"parameters": {...}}, {"args": {...}}, etc.
            // Unwrap only known wrapper keys that contain an object value.
            if obj.len() == 1 {
                let key = obj.keys().next().unwrap().clone();
                if WRAPPER_KEYS.contains(&key.as_str()) && obj[&key].is_object() {
                    obj.remove(&key).unwrap()
                } else {
                    Value::Object(obj)
                }
            } else {
                Value::Object(obj)
            }
        } else {
            action
        };

        tool_calls.push(ToolCall {
            id: format!("call_{}", i),
            name,
            arguments,
        });
    }

    Ok((situation, tool_calls))
}

/// Parse error for action extraction.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct ParseError(pub String);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolDef;

    fn mock_tools() -> Vec<ToolDef> {
        vec![
            ToolDef {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" }
                    },
                    "required": ["path"]
                }),
            },
            ToolDef {
                name: "bash".into(),
                description: "Run command".into(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" }
                    },
                    "required": ["command"]
                }),
            },
        ]
    }

    #[test]
    fn build_schema_has_one_of() {
        let schema = build_action_schema(&mock_tools());
        let items = &schema["properties"]["actions"]["items"];
        let one_of = items["oneOf"].as_array().unwrap();
        assert_eq!(one_of.len(), 2);

        // First variant has tool_name const
        let first = &one_of[0];
        assert_eq!(first["properties"]["tool_name"]["const"], "read_file");
        assert!(first["properties"]["path"].is_object());
    }

    #[test]
    fn build_schema_has_situation_and_task() {
        let schema = build_action_schema(&mock_tools());
        assert!(schema["properties"]["situation"].is_object());
        assert!(schema["properties"]["task"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("situation")));
    }

    #[test]
    fn parse_action_extracts_calls() {
        let raw = r#"{
            "situation": "need to read a file",
            "task": ["read main.rs"],
            "actions": [
                {"tool_name": "read_file", "path": "/src/main.rs"},
                {"tool_name": "bash", "command": "ls -la"}
            ]
        }"#;
        let (situation, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(situation, "need to read a file");
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "/src/main.rs");
        assert_eq!(calls[1].name, "bash");
        // tool_name should be stripped from args
        assert!(calls[0].arguments.get("tool_name").is_none());
    }

    #[test]
    fn parse_action_empty_actions() {
        let raw = r#"{"situation": "done", "task": [], "actions": []}"#;
        let (_, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_action_markdown_wrapped() {
        let raw = "```json\n{\"situation\": \"ok\", \"task\": [], \"actions\": [{\"tool_name\": \"bash\", \"command\": \"pwd\"}]}\n```";
        let (_, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
    }

    #[test]
    fn parse_action_unwraps_parameters_wrapper() {
        // Gemini wraps args in {"parameters": {...}}
        let raw = r#"{"situation": "reading", "task": [], "actions": [
            {"tool_name": "read_file", "parameters": {"path": "/main.rs"}},
            {"tool_name": "bash", "params": {"command": "ls"}}
        ]}"#;
        let (_, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(calls[0].arguments["path"], "/main.rs");
        assert_eq!(calls[1].arguments["command"], "ls");
    }

    #[test]
    fn parse_action_keeps_single_real_arg() {
        // Single real arg should NOT be unwrapped
        let raw = r#"{"situation": "ok", "task": [], "actions": [
            {"tool_name": "bash", "command": "ls"}
        ]}"#;
        let (_, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(calls[0].arguments["command"], "ls");
    }
}
