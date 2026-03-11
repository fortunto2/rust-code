//! SchemaSimplifier — converts JSON Schema to human-readable text.
//!
//! Used by FlexibleAgent to describe tool parameters in the system prompt
//! without overwhelming weak models with raw JSON Schema syntax.

use serde_json::Value;

/// Convert a JSON Schema to human-readable text description.
///
/// Input: `{"type": "object", "properties": {"path": {"type": "string", "description": "File path"}}, "required": ["path"]}`
/// Output: `- path (required, string): File path`
pub fn simplify(schema: &Value) -> String {
    let mut lines = Vec::new();
    simplify_object(schema, &mut lines, 0);
    lines.join("\n")
}

fn simplify_object(schema: &Value, lines: &mut Vec<String>, indent: usize) {
    let required: Vec<&str> = schema
        .get("required")
        .and_then(|r| r.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();

    let properties = match schema.get("properties").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return,
    };

    let prefix = "  ".repeat(indent);
    for (name, prop) in properties {
        let req_label = if required.contains(&name.as_str()) {
            "required"
        } else {
            "optional"
        };

        let type_str = format_type(prop);
        let constraints = format_constraints(prop);
        let desc = prop
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or("");

        let mut parts = vec![req_label.to_string(), type_str];
        if !constraints.is_empty() {
            parts.push(constraints);
        }
        let suffix = if desc.is_empty() {
            String::new()
        } else {
            format!(": {}", desc)
        };

        lines.push(format!(
            "{}- {} ({}){}",
            prefix,
            name,
            parts.join(", "),
            suffix
        ));

        // Recurse into nested objects
        if prop.get("type").and_then(|t| t.as_str()) == Some("object") {
            simplify_object(prop, lines, indent + 1);
        }

        // Array items
        if prop.get("type").and_then(|t| t.as_str()) == Some("array") {
            if let Some(items) = prop.get("items") {
                if items.get("type").and_then(|t| t.as_str()) == Some("object") {
                    let item_prefix = "  ".repeat(indent + 1);
                    lines.push(format!("{}  Each item:", item_prefix));
                    simplify_object(items, lines, indent + 2);
                }
            }
        }
    }
}

fn format_type(prop: &Value) -> String {
    match prop.get("type").and_then(|t| t.as_str()) {
        Some("array") => {
            let item_type = prop
                .get("items")
                .and_then(|i| i.get("type"))
                .and_then(|t| t.as_str())
                .unwrap_or("any");
            format!("array of {}", item_type)
        }
        Some(t) => t.to_string(),
        None => {
            // Check for enum
            if prop.get("enum").is_some() {
                "enum".to_string()
            } else if prop.get("oneOf").is_some() || prop.get("anyOf").is_some() {
                "union".to_string()
            } else {
                "any".to_string()
            }
        }
    }
}

fn format_constraints(prop: &Value) -> String {
    let mut parts = Vec::new();

    if let Some(Value::Array(variants)) = prop.get("enum") {
        let vals: Vec<String> = variants
            .iter()
            .map(|v| match v {
                Value::String(s) => format!("\"{}\"", s),
                _ => v.to_string(),
            })
            .collect();
        parts.push(format!("one of: {}", vals.join(" | ")));
    }

    if let Some(min) = prop.get("minimum") {
        parts.push(format!("min: {}", min));
    }
    if let Some(max) = prop.get("maximum") {
        parts.push(format!("max: {}", max));
    }
    if let Some(min) = prop.get("minLength") {
        parts.push(format!("minLength: {}", min));
    }
    if let Some(max) = prop.get("maxLength") {
        parts.push(format!("maxLength: {}", max));
    }
    if let Some(pat) = prop.get("pattern").and_then(|p| p.as_str()) {
        parts.push(format!("pattern: {}", pat));
    }
    if let Some(def) = prop.get("default") {
        parts.push(format!("default: {}", def));
    }

    parts.join(", ")
}

/// Simplify an entire tool definition into a human-readable block.
pub fn simplify_tool(name: &str, description: &str, schema: &Value) -> String {
    let params = simplify(schema);
    if params.is_empty() {
        format!("### {}\n{}\nNo parameters.", name, description)
    } else {
        format!("### {}\n{}\nParameters:\n{}", name, description, params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn simplify_basic_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path to read" },
                "line": { "type": "integer", "description": "Start line" }
            },
            "required": ["path"]
        });
        let result = simplify(&schema);
        assert!(result.contains("- path (required, string): File path to read"));
        assert!(result.contains("- line (optional, integer): Start line"));
    }

    #[test]
    fn simplify_with_enum() {
        let schema = json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["read", "write", "append"],
                    "description": "File open mode"
                }
            },
            "required": ["mode"]
        });
        let result = simplify(&schema);
        assert!(result.contains("one of:"));
        assert!(result.contains("\"read\""));
    }

    #[test]
    fn simplify_nested_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "config": {
                    "type": "object",
                    "properties": {
                        "timeout": { "type": "integer", "description": "Timeout in ms" }
                    },
                    "required": ["timeout"]
                }
            }
        });
        let result = simplify(&schema);
        assert!(result.contains("- config (optional, object)"));
        assert!(result.contains("  - timeout (required, integer): Timeout in ms"));
    }

    #[test]
    fn simplify_array_type() {
        let schema = json!({
            "type": "object",
            "properties": {
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of tags"
                }
            }
        });
        let result = simplify(&schema);
        assert!(result.contains("array of string"));
    }

    #[test]
    fn simplify_constraints() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "description": "Item count"
                }
            },
            "required": ["count"]
        });
        let result = simplify(&schema);
        assert!(result.contains("min: 1"));
        assert!(result.contains("max: 100"));
    }

    #[test]
    fn simplify_empty_schema() {
        let schema = json!({"type": "object"});
        let result = simplify(&schema);
        assert!(result.is_empty());
    }

    #[test]
    fn simplify_tool_full() {
        let schema = json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command" }
            },
            "required": ["command"]
        });
        let result = simplify_tool("bash", "Run a shell command", &schema);
        assert!(result.contains("### bash"));
        assert!(result.contains("Run a shell command"));
        assert!(result.contains("- command (required, string): Shell command"));
    }
}
