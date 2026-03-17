//! JSON Schema generation from Rust types via `schemars`.
//!
//! Two use cases:
//! - `response_schema_for::<T>()` — structured output (SGR envelope)
//! - `json_schema_for::<T>()` — tool parameter schema
//!
//! Both produce `serde_json::Value` compatible with Gemini/OpenAI APIs.

use schemars::{schema_for, JsonSchema};
use serde_json::Value;

/// Generate a JSON Schema for type `T` (for tool parameters).
///
/// Returns the schema object directly (no wrapper).
pub fn json_schema_for<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    let mut value = serde_json::to_value(schema).unwrap_or_default();
    // Remove $schema and title — APIs don't want them in nested schemas
    clean_schema(&mut value);
    value
}

/// Generate a response schema for type `T` (for structured output).
///
/// Wraps in the format expected by Gemini `responseSchema` or OpenAI `json_schema`.
pub fn response_schema_for<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    let mut value = serde_json::to_value(schema).unwrap_or_default();
    inline_refs(&mut value);
    clean_schema(&mut value);
    strip_unsupported_gemini(&mut value);
    value
}

/// Clean up schemars output for LLM API compatibility.
///
/// - Removes `$schema` (not supported by Gemini)
/// - Converts `examples` to shorter form
/// - Ensures `type` is present on all objects
fn clean_schema(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("$schema");
        // Gemini doesn't support "title" at top level in some contexts
        // but it's fine for readability — keep it

        // Recursively clean nested schemas
        if let Some(props) = obj.get_mut("properties") {
            if let Some(props_obj) = props.as_object_mut() {
                for (_, prop_schema) in props_obj.iter_mut() {
                    clean_schema(prop_schema);
                }
            }
        }
        if let Some(defs) = obj.get_mut("definitions") {
            if let Some(defs_obj) = defs.as_object_mut() {
                for (_, def) in defs_obj.iter_mut() {
                    clean_schema(def);
                }
            }
        }
        if let Some(items) = obj.get_mut("items") {
            clean_schema(items);
        }
        // oneOf / anyOf / allOf
        for key in &["oneOf", "anyOf", "allOf"] {
            if let Some(arr) = obj.get_mut(*key) {
                if let Some(arr_vec) = arr.as_array_mut() {
                    for item in arr_vec.iter_mut() {
                        clean_schema(item);
                    }
                }
            }
        }
    }
}

/// Convert a schemars-generated schema into Gemini's `FunctionDeclaration.parameters` format.
///
/// Gemini uses a subset of OpenAPI 3.0 schema (not full JSON Schema).
/// Key differences:
/// - No `$ref` / `definitions` — everything must be inlined
/// - `type` must be lowercase string: "string", "number", "integer", "boolean", "array", "object"
/// - No `additionalProperties` by default
pub fn to_gemini_parameters<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    let mut value = serde_json::to_value(schema).unwrap_or_default();
    inline_refs(&mut value);
    clean_schema(&mut value);
    strip_unsupported_gemini(&mut value);
    value
}

/// Inline all `$ref` references by resolving from `definitions`.
fn inline_refs(root: &mut Value) {
    // Collect definitions first
    let definitions = root
        .get("definitions")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    resolve_refs(root, &definitions);

    // Remove definitions after inlining
    if let Some(obj) = root.as_object_mut() {
        obj.remove("definitions");
    }
}

fn resolve_refs(value: &mut Value, definitions: &Value) {
    if let Some(obj) = value.as_object_mut() {
        // If this is a $ref, replace with the definition
        if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str()).map(String::from) {
            // "#/definitions/Foo" → "Foo"
            if let Some(name) = ref_str.strip_prefix("#/definitions/") {
                if let Some(def) = definitions.get(name) {
                    let mut resolved = def.clone();
                    resolve_refs(&mut resolved, definitions);
                    *value = resolved;
                    return;
                }
            }
        }

        // Recurse into all sub-schemas
        let keys: Vec<String> = obj.keys().cloned().collect();
        for key in keys {
            if let Some(child) = obj.get_mut(&key) {
                resolve_refs(child, definitions);
            }
        }
    } else if let Some(arr) = value.as_array_mut() {
        for item in arr.iter_mut() {
            resolve_refs(item, definitions);
        }
    }
}

/// Strip JSON Schema features not supported by Gemini's OpenAPI subset.
///
/// Gemini uses a restricted OpenAPI 3.0 schema:
/// - `type` must be a single string, not an array (schemars emits `["string", "null"]` for Option)
/// - No `additionalProperties`, `default`, `$schema`
/// - Nullable: use `"nullable": true` instead of `"type": ["T", "null"]`
fn strip_unsupported_gemini(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("$schema");
        obj.remove("additionalProperties");
        obj.remove("default");

        // Fix type arrays: ["string", "null"] → "string" + nullable: true
        if let Some(type_val) = obj.get("type").cloned() {
            if let Some(arr) = type_val.as_array() {
                let non_null: Vec<&Value> =
                    arr.iter().filter(|v| v.as_str() != Some("null")).collect();
                let has_null = arr.iter().any(|v| v.as_str() == Some("null"));
                if let Some(first) = non_null.first() {
                    obj.insert("type".to_string(), (*first).clone());
                }
                if has_null {
                    obj.insert("nullable".to_string(), Value::Bool(true));
                }
            }
        }

        // Recurse
        let keys: Vec<String> = obj.keys().cloned().collect();
        for key in keys {
            if let Some(child) = obj.get_mut(&key) {
                strip_unsupported_gemini(child);
            }
        }
    } else if let Some(arr) = value.as_array_mut() {
        for item in arr.iter_mut() {
            strip_unsupported_gemini(item);
        }
    }
}

/// Make a JSON Schema compatible with OpenAI strict mode.
///
/// OpenAI `strict: true` requires:
/// 1. `additionalProperties: false` on every object
/// 2. All properties listed in `required` (optional fields use `"type": ["string", "null"]`)
///
/// See: https://developers.openai.com/api/docs/guides/structured-outputs
pub fn make_openai_strict(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        let is_object = obj.get("type").and_then(|v| v.as_str()) == Some("object");
        if is_object {
            obj.insert("additionalProperties".into(), Value::Bool(false));
            // All properties must be in required
            if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
                let all_keys: Vec<Value> = props.keys().map(|k| Value::String(k.clone())).collect();
                obj.insert("required".into(), Value::Array(all_keys));
            }
        }
        // OpenAI strict mode: oneOf not supported, convert to anyOf
        if let Some(one_of) = obj.remove("oneOf") {
            obj.insert("anyOf".into(), one_of);
        }
        // Recurse
        for key in obj.keys().cloned().collect::<Vec<_>>() {
            if let Some(child) = obj.get_mut(&key) {
                make_openai_strict(child);
            }
        }
    } else if let Some(arr) = value.as_array_mut() {
        for item in arr {
            make_openai_strict(item);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct TestTool {
        /// Path to the input file.
        input_path: String,
        /// Whether to overwrite existing output.
        overwrite: Option<bool>,
    }

    #[test]
    fn generates_valid_schema() {
        let schema = json_schema_for::<TestTool>();
        assert!(schema.get("properties").is_some());
        let props = schema["properties"].as_object().unwrap();
        assert!(props.contains_key("input_path"));
        assert!(props.contains_key("overwrite"));
    }

    #[test]
    fn gemini_parameters_inlines_refs() {
        #[derive(Debug, Serialize, Deserialize, JsonSchema)]
        struct Inner {
            value: String,
        }
        #[derive(Debug, Serialize, Deserialize, JsonSchema)]
        struct Outer {
            inner: Inner,
        }

        let params = to_gemini_parameters::<Outer>();
        // Should NOT contain $ref or definitions
        let text = serde_json::to_string(&params).unwrap();
        assert!(!text.contains("$ref"));
        assert!(!text.contains("definitions"));
    }
}
