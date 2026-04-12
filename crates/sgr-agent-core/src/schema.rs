//! JSON Schema generation from Rust types via `schemars`.
//!
//! Two use cases:
//! - `response_schema_for::<T>()` — structured output (SGR envelope)
//! - `json_schema_for::<T>()` — tool parameter schema
//!
//! Both produce `serde_json::Value` compatible with Gemini/OpenAI APIs.

use schemars::{JsonSchema, schema_for};
use serde_json::Value;

/// Generate a JSON Schema for type `T` (for tool parameters).
///
/// Returns the schema object directly (no wrapper).
pub fn json_schema_for<T: JsonSchema>() -> Value {
    let schema = schema_for!(T);
    let mut value = serde_json::to_value(schema).unwrap_or_default();
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
fn clean_schema(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("$schema");

        if obj.get("type").and_then(|v| v.as_str()) == Some("object")
            && !obj.contains_key("properties")
        {
            obj.insert("properties".into(), Value::Object(serde_json::Map::new()));
        }

        if let Some(props) = obj.get_mut("properties")
            && let Some(props_obj) = props.as_object_mut()
        {
            for (_, prop_schema) in props_obj.iter_mut() {
                clean_schema(prop_schema);
            }
        }
        if let Some(defs) = obj.get_mut("definitions")
            && let Some(defs_obj) = defs.as_object_mut()
        {
            for (_, def) in defs_obj.iter_mut() {
                clean_schema(def);
            }
        }
        if let Some(items) = obj.get_mut("items") {
            clean_schema(items);
        }
        for key in &["oneOf", "anyOf", "allOf"] {
            if let Some(arr) = obj.get_mut(*key)
                && let Some(arr_vec) = arr.as_array_mut()
            {
                for item in arr_vec.iter_mut() {
                    clean_schema(item);
                }
            }
        }
    }
}

/// Convert a schemars-generated schema into Gemini's `FunctionDeclaration.parameters` format.
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
    let definitions = root
        .get("definitions")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));
    resolve_refs(root, &definitions);
    if let Some(obj) = root.as_object_mut() {
        obj.remove("definitions");
    }
}

fn resolve_refs(value: &mut Value, definitions: &Value) {
    if let Some(obj) = value.as_object_mut() {
        if let Some(ref_str) = obj.get("$ref").and_then(|v| v.as_str()).map(String::from)
            && let Some(name) = ref_str.strip_prefix("#/definitions/")
            && let Some(def) = definitions.get(name)
        {
            let mut resolved = def.clone();
            resolve_refs(&mut resolved, definitions);
            *value = resolved;
            return;
        }
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

fn strip_unsupported_gemini(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        obj.remove("$schema");
        obj.remove("additionalProperties");
        obj.remove("default");

        if let Some(type_val) = obj.get("type").cloned()
            && let Some(arr) = type_val.as_array()
        {
            let non_null: Vec<&Value> = arr.iter().filter(|v| v.as_str() != Some("null")).collect();
            let has_null = arr.iter().any(|v| v.as_str() == Some("null"));
            if let Some(first) = non_null.first() {
                obj.insert("type".to_string(), (*first).clone());
            }
            if has_null {
                obj.insert("nullable".to_string(), Value::Bool(true));
            }
        }

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
pub fn make_openai_strict(value: &mut Value) {
    if let Some(obj) = value.as_object_mut() {
        let is_object = obj.get("type").and_then(|v| v.as_str()) == Some("object");
        if is_object {
            obj.insert("additionalProperties".into(), Value::Bool(false));

            if let Some(props) = obj.get_mut("properties").and_then(|v| v.as_object_mut()) {
                for (_key, prop) in props.iter_mut() {
                    if let Some(prop_obj) = prop.as_object_mut()
                        && prop_obj.remove("nullable").and_then(|v| v.as_bool()) == Some(true)
                        && let Some(type_val) = prop_obj.remove("type")
                    {
                        let desc = prop_obj.remove("description");
                        let any_of = vec![
                            serde_json::json!({"type": type_val}),
                            serde_json::json!({"type": "null"}),
                        ];
                        let mut wrapper = serde_json::Map::new();
                        wrapper.insert("anyOf".into(), Value::Array(any_of));
                        if let Some(d) = desc {
                            wrapper.insert("description".into(), d);
                        }
                        *prop = Value::Object(wrapper);
                    }
                }
            }

            if let Some(props) = obj.get("properties").and_then(|v| v.as_object()) {
                let all_keys: Vec<Value> = props.keys().map(|k| Value::String(k.clone())).collect();
                obj.insert("required".into(), Value::Array(all_keys));
            }
        }
        if let Some(one_of) = obj.remove("oneOf") {
            obj.insert("anyOf".into(), one_of);
        }
        if let Some(all_of) = obj.remove("allOf")
            && let Some(arr) = all_of.as_array()
        {
            if arr.len() == 1 {
                if let Some(inner) = arr[0].as_object() {
                    for (k, v) in inner {
                        obj.entry(k.clone()).or_insert(v.clone());
                    }
                }
            } else {
                obj.insert("anyOf".into(), all_of);
            }
        }
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
        input_path: String,
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
    fn strict_makes_all_required_and_nullable() {
        let mut schema = to_gemini_parameters::<TestTool>();
        make_openai_strict(&mut schema);
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&Value::String("input_path".into())));
        assert!(required.contains(&Value::String("overwrite".into())));
        assert_eq!(schema["additionalProperties"], false);
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
        let text = serde_json::to_string(&params).unwrap();
        assert!(!text.contains("$ref"));
        assert!(!text.contains("definitions"));
    }
}
