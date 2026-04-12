//! Tool definitions — typed Rust structs → function declarations for LLM APIs.

use crate::schema::to_gemini_parameters;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A tool definition ready for LLM API submission.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Create a tool definition from a typed struct.
pub fn tool<T: JsonSchema + DeserializeOwned>(name: &str, description: &str) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        parameters: to_gemini_parameters::<T>(),
    }
}

impl ToolDef {
    pub fn to_gemini(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "parameters": self.parameters,
        })
    }

    pub fn to_openai(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.parameters,
            }
        })
    }

    pub fn parse_args<T: DeserializeOwned>(&self, args: &Value) -> Result<T, serde_json::Error> {
        serde_json::from_value(args.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, JsonSchema)]
    struct MockTool {
        input_path: String,
        quality: Option<f64>,
    }

    #[test]
    fn tool_generates_gemini_format() {
        let t = tool::<MockTool>("mock_tool", "A mock tool");
        let gemini = t.to_gemini();
        assert_eq!(gemini["name"], "mock_tool");
        assert!(gemini["parameters"]["properties"]["input_path"].is_object());
    }

    #[test]
    fn tool_generates_openai_format() {
        let t = tool::<MockTool>("mock_tool", "A mock tool");
        let openai = t.to_openai();
        assert_eq!(openai["type"], "function");
        assert_eq!(openai["function"]["name"], "mock_tool");
    }
}
