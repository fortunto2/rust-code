//! Tool definitions — typed Rust structs → function declarations for LLM APIs.
//!
//! ```ignore
//! #[derive(Serialize, Deserialize, JsonSchema)]
//! /// Analyze a video file with scene detection and scoring.
//! struct AnalysisTask {
//!     /// Path to the video file.
//!     input_path: String,
//!     /// Scene detection algorithm.
//!     scene_algo: Option<String>,
//! }
//!
//! let tools = vec![
//!     tool::<AnalysisTask>("analysis_operation", "Run video analysis"),
//!     tool::<FfmpegTask>("ffmpeg_operation", "FFmpeg conversion"),
//! ];
//! ```

use crate::schema::to_gemini_parameters;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// A tool definition ready for LLM API submission.
#[derive(Debug, Clone)]
pub struct ToolDef {
    /// Function name (e.g. "analysis_operation").
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema for parameters (inlined, no $ref).
    pub parameters: Value,
}

/// Create a tool definition from a typed struct.
///
/// The struct must implement `JsonSchema` + `DeserializeOwned`.
/// Schema is generated at call time (cheap — just serde).
pub fn tool<T: JsonSchema + DeserializeOwned>(name: &str, description: &str) -> ToolDef {
    ToolDef {
        name: name.to_string(),
        description: description.to_string(),
        parameters: to_gemini_parameters::<T>(),
    }
}

impl ToolDef {
    /// Convert to Gemini `FunctionDeclaration` format.
    pub fn to_gemini(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "parameters": self.parameters,
        })
    }

    /// Convert to OpenAI `tools[]` format.
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

    /// Parse tool call arguments into the typed struct.
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
        assert_eq!(gemini["description"], "A mock tool");
        assert!(gemini["parameters"]["properties"]["input_path"].is_object());
    }

    #[test]
    fn tool_generates_openai_format() {
        let t = tool::<MockTool>("mock_tool", "A mock tool");
        let openai = t.to_openai();
        assert_eq!(openai["type"], "function");
        assert_eq!(openai["function"]["name"], "mock_tool");
    }

    #[test]
    fn parse_args_works() {
        let t = tool::<MockTool>("mock_tool", "test");
        let args = serde_json::json!({"input_path": "/video.mp4", "quality": 0.8});
        let parsed: MockTool = t.parse_args(&args).unwrap();
        assert_eq!(parsed.input_path, "/video.mp4");
        assert_eq!(parsed.quality, Some(0.8));
    }
}
