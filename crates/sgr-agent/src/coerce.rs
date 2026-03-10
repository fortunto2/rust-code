//! Fuzzy type coercion for LLM outputs.
//!
//! LLMs often return "42" instead of 42, "true" instead of true, "redd" instead of "Red".
//! This module coerces `serde_json::Value` to match the expected JSON Schema before
//! deserializing into typed Rust structs.
//!
//! Lighter alternative to BAML's full type system — works with `serde_json::Value` directly.

use serde_json::Value;

/// Coerce a JSON value to better match the expected schema.
///
/// Applies fuzzy conversions recursively:
/// - `"42"` → `42` when schema expects number
/// - `"true"/"yes"/"1"` → `true` when schema expects bool
/// - `3.7` → `4` when schema expects integer
/// - String fuzzy matching for enum variants
pub fn coerce_value(value: &mut Value, schema: &Value) {
    let schema_type = schema.get("type").and_then(|t| t.as_str()).unwrap_or("");

    match schema_type {
        "integer" => coerce_to_integer(value),
        "number" => coerce_to_number(value),
        "boolean" => coerce_to_bool(value),
        "string" => coerce_to_string(value, schema),
        "array" => coerce_array(value, schema),
        "object" => coerce_object(value, schema),
        _ => {}
    }
}

/// Coerce value to integer: "42" → 42, 3.7 → 4
fn coerce_to_integer(value: &mut Value) {
    match value {
        Value::String(s) => {
            let trimmed = s.trim().trim_end_matches(',');
            if let Ok(n) = trimmed.parse::<i64>() {
                *value = Value::Number(n.into());
            } else if let Ok(f) = trimmed.parse::<f64>() {
                *value = Value::Number((f.round() as i64).into());
            }
        }
        Value::Number(n) => {
            if let Some(f) = n.as_f64() {
                if f.fract() != 0.0 {
                    *value = Value::Number((f.round() as i64).into());
                }
            }
        }
        Value::Bool(b) => {
            *value = Value::Number(if *b { 1.into() } else { 0.into() });
        }
        _ => {}
    }
}

/// Coerce value to number: "3.14" → 3.14
fn coerce_to_number(value: &mut Value) {
    if let Value::String(s) = value {
        let trimmed = s.trim().trim_end_matches(',');
        if let Ok(f) = trimmed.parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(f) {
                *value = Value::Number(n);
            }
        }
    }
}

/// Coerce value to bool: "true"/"yes"/"1"/1 → true
fn coerce_to_bool(value: &mut Value) {
    match value {
        Value::String(s) => {
            match s.to_lowercase().trim() {
                "true" | "yes" | "1" | "on" | "y" => *value = Value::Bool(true),
                "false" | "no" | "0" | "off" | "n" => *value = Value::Bool(false),
                _ => {}
            }
        }
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                *value = Value::Bool(i != 0);
            }
        }
        _ => {}
    }
}

/// Coerce string with enum constraint: fuzzy match against allowed values.
fn coerce_to_string(value: &mut Value, schema: &Value) {
    // Only apply enum fuzzy matching if schema has enum constraint
    let enum_values = match schema.get("enum").and_then(|e| e.as_array()) {
        Some(vals) => vals,
        None => return,
    };

    let raw = match value.as_str() {
        Some(s) => s.to_string(),
        None => return,
    };

    // Exact match (case-insensitive)
    for v in enum_values {
        if let Some(expected) = v.as_str() {
            if raw.eq_ignore_ascii_case(expected) {
                *value = Value::String(expected.to_string());
                return;
            }
        }
    }

    // Fuzzy match using string distance
    let candidates: Vec<&str> = enum_values.iter().filter_map(|v| v.as_str()).collect();
    if let Some(best) = fuzzy_match(&raw, &candidates, 0.6) {
        *value = Value::String(best.to_string());
    }
}

/// Coerce array elements.
fn coerce_array(value: &mut Value, schema: &Value) {
    // Try to coerce a comma-separated string into an array
    if let Value::String(s) = value {
        let items: Vec<Value> = s
            .split(',')
            .map(|item| Value::String(item.trim().to_string()))
            .collect();
        if items.len() > 1 {
            *value = Value::Array(items);
        }
    }

    if let Value::Array(arr) = value {
        if let Some(items_schema) = schema.get("items") {
            for item in arr.iter_mut() {
                coerce_value(item, items_schema);
            }
        }
    }
}

/// Coerce object properties.
///
/// Also fills in missing required fields with sensible defaults:
/// - missing array → `[]`
/// - missing string → `""`
/// - missing object → `{}`
/// This matches BAML's behavior for streaming/truncated partial responses.
fn coerce_object(value: &mut Value, schema: &Value) {
    if let Value::Object(map) = value {
        if let Some(props) = schema.get("properties").and_then(|p| p.as_object()) {
            for (key, prop_schema) in props {
                if let Some(field_value) = map.get_mut(key) {
                    coerce_value(field_value, prop_schema);
                } else {
                    // Fill missing required fields with defaults
                    let prop_type = prop_schema.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let default_val = match prop_type {
                        "array" => Some(Value::Array(vec![])),
                        "string" => Some(Value::String(String::new())),
                        "object" => Some(Value::Object(serde_json::Map::new())),
                        _ => None,
                    };
                    if let Some(val) = default_val {
                        map.insert(key.clone(), val);
                    }
                }
            }
        }
    }
}

/// Find the best fuzzy match for `input` among `candidates`.
/// Returns None if best similarity is below `threshold`.
fn fuzzy_match<'a>(input: &str, candidates: &[&'a str], threshold: f64) -> Option<&'a str> {
    let input_lower = input.to_lowercase();
    let mut best: Option<(&str, f64)> = None;

    for &candidate in candidates {
        let sim = strsim::normalized_levenshtein(&input_lower, &candidate.to_lowercase());
        if sim >= threshold && (best.is_none() || sim > best.unwrap().1) {
            best = Some((candidate, sim));
        }
    }

    best.map(|(s, _)| s)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn coerce_string_to_int() {
        let schema = json!({"type": "integer"});
        let mut value = json!("42");
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!(42));
    }

    #[test]
    fn coerce_float_to_int() {
        let schema = json!({"type": "integer"});
        let mut value = json!(3.7);
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!(4));
    }

    #[test]
    fn coerce_string_to_number() {
        let schema = json!({"type": "number"});
        let mut value = json!("3.14");
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!(3.14));
    }

    #[test]
    fn coerce_string_to_bool() {
        let schema = json!({"type": "boolean"});

        let cases = vec![
            ("true", true),
            ("True", true),
            ("yes", true),
            ("YES", true),
            ("1", true),
            ("false", false),
            ("no", false),
            ("0", false),
        ];

        for (input, expected) in cases {
            let mut value = json!(input);
            coerce_value(&mut value, &schema);
            assert_eq!(value, json!(expected), "Failed for input: {}", input);
        }
    }

    #[test]
    fn coerce_number_to_bool() {
        let schema = json!({"type": "boolean"});
        let mut value = json!(1);
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!(true));
    }

    #[test]
    fn coerce_enum_exact_case_insensitive() {
        let schema = json!({"type": "string", "enum": ["Red", "Blue", "Green"]});
        let mut value = json!("red");
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!("Red"));
    }

    #[test]
    fn coerce_enum_fuzzy() {
        let schema = json!({"type": "string", "enum": ["Red", "Blue", "Green"]});
        let mut value = json!("Gren");
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!("Green"));
    }

    #[test]
    fn coerce_enum_no_match_below_threshold() {
        let schema = json!({"type": "string", "enum": ["Red", "Blue", "Green"]});
        let mut value = json!("xyz");
        coerce_value(&mut value, &schema);
        // No match — stays as is
        assert_eq!(value, json!("xyz"));
    }

    #[test]
    fn coerce_object_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "count": {"type": "integer"},
                "active": {"type": "boolean"},
                "color": {"type": "string", "enum": ["Red", "Blue"]}
            }
        });
        let mut value = json!({"count": "5", "active": "yes", "color": "blue"});
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!({"count": 5, "active": true, "color": "Blue"}));
    }

    #[test]
    fn coerce_array_items() {
        let schema = json!({"type": "array", "items": {"type": "integer"}});
        let mut value = json!(["1", "2", "3"]);
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!([1, 2, 3]));
    }

    #[test]
    fn coerce_comma_separated_string_to_array() {
        let schema = json!({"type": "array", "items": {"type": "string"}});
        let mut value = json!("apple, banana, cherry");
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!(["apple", "banana", "cherry"]));
    }

    #[test]
    fn coerce_bool_to_int() {
        let schema = json!({"type": "integer"});
        let mut value = json!(true);
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!(1));
    }

    #[test]
    fn no_coerce_when_already_correct() {
        let schema = json!({"type": "integer"});
        let mut value = json!(42);
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!(42));
    }

    #[test]
    fn fuzzy_match_works() {
        let candidates = vec!["Red", "Blue", "Green", "Yellow"];
        assert_eq!(fuzzy_match("red", &candidates, 0.6), Some("Red"));
        assert_eq!(fuzzy_match("Gren", &candidates, 0.6), Some("Green"));
        assert_eq!(fuzzy_match("Blu", &candidates, 0.6), Some("Blue"));
        assert_eq!(fuzzy_match("xyz", &candidates, 0.6), None);
    }

    #[test]
    fn coerce_nested_object() {
        let schema = json!({
            "type": "object",
            "properties": {
                "inner": {
                    "type": "object",
                    "properties": {
                        "score": {"type": "number"}
                    }
                }
            }
        });
        let mut value = json!({"inner": {"score": "0.95"}});
        coerce_value(&mut value, &schema);
        assert_eq!(value, json!({"inner": {"score": 0.95}}));
    }
}
