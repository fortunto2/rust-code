//! Dynamic flat action schema builder — generates a single object JSON Schema
//! from tool definitions at runtime. Used by SgrAgent for structured output.
//!
//! Instead of `anyOf` discriminated unions (which break OpenAI constrained decoding),
//! uses a flat schema: `tool_name` as string enum + all params as nullable fields.

use crate::tool::ToolDef;
use crate::types::ToolCall;
use serde_json::Value;
use std::collections::BTreeMap;

/// Build a flat JSON Schema from tool definitions.
///
/// The schema is already OpenAI strict-compatible:
/// - All properties are in `required`
/// - Non-universal params use `anyOf [type, null]`
/// - `additionalProperties: false`
///
/// IMPORTANT: Do NOT run `ensure_strict` on this schema.
pub fn build_action_schema(tools: &[ToolDef]) -> Value {
    let tool_names: Vec<Value> = tools
        .iter()
        .map(|t| Value::String(t.name.clone()))
        .collect();

    // Collect all unique parameter names across all tools with their schemas.
    // If multiple tools define the same param name, merge descriptions.
    let mut all_params: BTreeMap<String, Value> = BTreeMap::new();
    let mut param_required_by: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for t in tools {
        if let Some(props) = t.parameters.get("properties").and_then(|p| p.as_object()) {
            let required_names: Vec<String> = t
                .parameters
                .get("required")
                .and_then(|r| r.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            for (name, schema) in props {
                all_params
                    .entry(name.clone())
                    .or_insert_with(|| schema.clone());
                if required_names.contains(name) {
                    param_required_by
                        .entry(name.clone())
                        .or_default()
                        .push(t.name.clone());
                }
            }
        }
    }

    // Build properties: tool_name enum + situation + plan + all params (nullable)
    let mut properties = serde_json::Map::new();

    properties.insert(
        "situation".into(),
        serde_json::json!({"type": "string", "description": "Brief assessment of current state"}),
    );
    properties.insert(
        "plan".into(),
        serde_json::json!({
            "type": "array",
            "items": {"type": "string"},
            "minItems": 1,
            "maxItems": 5,
            "description": "1-5 brief remaining steps"
        }),
    );
    properties.insert(
        "tool_name".into(),
        serde_json::json!({"type": "string", "enum": tool_names, "description": "Tool to execute"}),
    );

    // All tool params — wrapped in anyOf [type, null] so model can set unused params to null
    for (name, schema) in &all_params {
        let nullable = serde_json::json!({
            "anyOf": [schema, {"type": "null"}],
            "description": schema.get("description").and_then(|d| d.as_str()).unwrap_or("")
        });
        properties.insert(name.clone(), nullable);
    }

    // All properties are required (strict mode) — nullable handles optionality
    let required: Vec<Value> = properties
        .keys()
        .map(|k| Value::String(k.clone()))
        .collect();

    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}

/// Parse raw LLM output into tool calls.
/// Supports both flat format (tool_name at top level) and legacy nested format.
pub fn parse_action(raw: &str, _tools: &[ToolDef]) -> Result<(String, Vec<ToolCall>), ParseError> {
    let value: Value = match crate::flexible_parser::parse_flexible::<Value>(raw) {
        Ok(r) => r.value,
        Err(_) => serde_json::from_str::<Value>(raw).map_err(|e| ParseError(e.to_string()))?,
    };

    let situation = match value.get("situation") {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    };

    // Flat format: tool_name at top level
    if let Some(Value::String(tool_name)) = value.get("tool_name") {
        let mut args = serde_json::Map::new();
        if let Value::Object(obj) = &value {
            for (k, v) in obj {
                match k.as_str() {
                    "situation" | "plan" | "task" | "tool_name" => continue,
                    _ => {
                        // Skip null values (unused params from other tools)
                        if !v.is_null() {
                            args.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
        return Ok((
            situation,
            vec![ToolCall {
                id: "call_0".into(),
                name: tool_name.clone(),
                arguments: Value::Object(args),
            }],
        ));
    }

    // Legacy nested format: "action" object or "actions" array
    let actions: Vec<Value> = match value.get("action") {
        Some(Value::Object(_)) => vec![value["action"].clone()],
        _ => match value.get("actions") {
            Some(Value::Array(arr)) => arr.clone(),
            _ => Vec::new(),
        },
    };

    let mut tool_calls: Vec<ToolCall> = Vec::new();
    for (i, action) in actions.into_iter().enumerate() {
        let name = match action.get("tool_name") {
            Some(Value::String(s)) => s.clone(),
            _ => continue,
        };

        let arguments = if let Value::Object(mut obj) = action {
            obj.remove("tool_name");
            // Unwrap known wrapper keys (Gemini compat)
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

/// Known wrapper keys that Gemini uses to wrap tool arguments.
const WRAPPER_KEYS: &[&str] = &["parameters", "params", "args", "arguments"];

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
                        "path": { "type": "string", "description": "File path" }
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
                        "command": { "type": "string", "description": "Shell command" }
                    },
                    "required": ["command"]
                }),
            },
        ]
    }

    #[test]
    fn build_schema_flat_with_enum() {
        let schema = build_action_schema(&mock_tools());
        let tool_name = &schema["properties"]["tool_name"];
        let enums = tool_name["enum"].as_array().unwrap();
        assert_eq!(enums.len(), 2);
        assert!(enums.contains(&serde_json::json!("read_file")));
        assert!(enums.contains(&serde_json::json!("bash")));
        // All params nullable
        assert!(schema["properties"]["path"]["anyOf"].is_array());
        assert!(schema["properties"]["command"]["anyOf"].is_array());
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn build_schema_has_situation_and_plan() {
        let schema = build_action_schema(&mock_tools());
        assert!(schema["properties"]["situation"].is_object());
        assert!(schema["properties"]["plan"].is_object());
        assert_eq!(schema["properties"]["plan"]["maxItems"], 5);
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::json!("situation")));
        assert!(required.contains(&serde_json::json!("tool_name")));
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn parse_flat_action() {
        let raw = r#"{
            "situation": "need to read a file",
            "plan": ["read main.rs"],
            "tool_name": "read_file",
            "path": "/src/main.rs",
            "command": null
        }"#;
        let (situation, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(situation, "need to read a file");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments["path"], "/src/main.rs");
        // null values stripped
        assert!(calls[0].arguments.get("command").is_none());
    }

    #[test]
    fn parse_legacy_nested_action() {
        let raw = r#"{
            "situation": "need to read a file",
            "plan": ["read main.rs"],
            "action": {"tool_name": "read_file", "path": "/src/main.rs"}
        }"#;
        let (situation, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(situation, "need to read a file");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "read_file");
    }

    #[test]
    fn parse_legacy_actions_array() {
        let raw = r#"{
            "situation": "multi",
            "task": ["a", "b"],
            "actions": [
                {"tool_name": "read_file", "path": "/src/main.rs"},
                {"tool_name": "bash", "command": "ls -la"}
            ]
        }"#;
        let (_, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn parse_missing_action_returns_empty() {
        let raw = r#"{"situation": "thinking"}"#;
        let (situation, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(situation, "thinking");
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_markdown_wrapped() {
        let raw = "```json\n{\"situation\": \"ok\", \"plan\": [\"do it\"], \"tool_name\": \"bash\", \"command\": \"pwd\", \"path\": null}\n```";
        let (_, calls) = parse_action(raw, &mock_tools()).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "bash");
        assert_eq!(calls[0].arguments["command"], "pwd");
    }

    #[test]
    fn ensure_strict_skipped_for_pre_strict() {
        // build_action_schema produces schemas with additionalProperties:false.
        // OxideClient's structured_call detects this and skips ensure_strict,
        // because ensure_strict would break anyOf-nullable fields.
        let schema = build_action_schema(&mock_tools());

        // Schema is already strict-compatible
        assert_eq!(schema["additionalProperties"], false);

        // All properties are required
        let required = schema["required"].as_array().unwrap();
        let props = schema["properties"].as_object().unwrap();
        for key in props.keys() {
            assert!(
                required.contains(&Value::String(key.clone())),
                "Property '{}' must be in required list",
                key
            );
        }

        // Tool params use anyOf [type, null] (nullable) —
        // ensure_strict would corrupt these by wrapping again
        let path_prop = &schema["properties"]["path"];
        let any_of = path_prop["anyOf"].as_array().unwrap();
        assert_eq!(any_of.len(), 2, "path should have anyOf with 2 variants");
        let has_null = any_of
            .iter()
            .any(|v| v.get("type") == Some(&Value::String("null".into())));
        assert!(has_null, "path anyOf should include null variant");
    }
}
