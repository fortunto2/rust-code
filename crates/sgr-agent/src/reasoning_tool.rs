//! ReasoningToolBuilder — build a structured reasoning tool from custom fields.
//!
//! Equivalent to Python SGR's `NextStepToolsBuilder` pattern.
//! Agent defines reasoning schema fields, builder creates ToolDef.

use crate::tool::ToolDef;
use serde_json::{Value, json};

/// Builder for reasoning/think tools with custom schema fields.
///
/// ```ignore
/// let think = ReasoningToolBuilder::new("think")
///     .description("Reason about the task before acting")
///     .field("task_type", json!({"type": "string", "enum": ["search", "edit", "delete"]}))
///     .field("plan", json!({"type": "string"}))
///     .field("security", json!({"type": "string", "enum": ["safe", "blocked"]}))
///     .optional("confidence", json!({"type": "number"}))
///     .build();
/// ```
pub struct ReasoningToolBuilder {
    name: String,
    description: String,
    properties: serde_json::Map<String, Value>,
    required: Vec<String>,
}

impl ReasoningToolBuilder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: String::new(),
            properties: serde_json::Map::new(),
            required: Vec::new(),
        }
    }

    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Add a required field to the reasoning schema.
    pub fn field(mut self, name: impl Into<String>, schema: Value) -> Self {
        let name = name.into();
        self.required.push(name.clone());
        self.properties.insert(name, schema);
        self
    }

    /// Add an optional field (not in required array).
    pub fn optional(mut self, name: impl Into<String>, schema: Value) -> Self {
        self.properties.insert(name.into(), schema);
        self
    }

    /// Build the ToolDef.
    pub fn build(self) -> ToolDef {
        ToolDef {
            name: self.name,
            description: self.description,
            parameters: json!({
                "type": "object",
                "properties": self.properties,
                "required": self.required,
                "additionalProperties": false
            }),
        }
    }
}

/// Preset: minimal reasoning tool (situation + plan + done).
pub fn minimal_reasoning(name: &str) -> ToolDef {
    ReasoningToolBuilder::new(name)
        .description("Assess situation and plan next action")
        .field(
            "situation",
            json!({"type": "string", "description": "Current state assessment"}),
        )
        .field(
            "plan",
            json!({"type": "string", "description": "Next action to take"}),
        )
        .field(
            "done",
            json!({"type": "boolean", "description": "true when task complete"}),
        )
        .build()
}

/// Preset: agent reasoning with task routing (PAC1/CRM style).
pub fn routed_reasoning(name: &str, task_types: &[&str], security_levels: &[&str]) -> ToolDef {
    let tt_enum: Vec<Value> = task_types
        .iter()
        .map(|s| Value::String(s.to_string()))
        .collect();
    let sec_enum: Vec<Value> = security_levels
        .iter()
        .map(|s| Value::String(s.to_string()))
        .collect();

    ReasoningToolBuilder::new(name)
        .description("Reason about the task. ALWAYS call this AND an action tool together.")
        .field("task_type", json!({"type": "string", "enum": tt_enum}))
        .field("security", json!({"type": "string", "enum": sec_enum}))
        .field("reasoning", json!({"type": "string", "description": "What you observe + self-check (Am I repeating? Right file? Evidence?)"}))
        .field("next_action", json!({"type": "string", "description": "What you will do now and why"}))
        .optional("confidence", json!({"type": "number", "description": "0.0-1.0 how sure you are"}))
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_valid_schema() {
        let tool = ReasoningToolBuilder::new("think")
            .description("Test reasoning")
            .field("plan", json!({"type": "string"}))
            .field("done", json!({"type": "boolean"}))
            .optional("confidence", json!({"type": "number"}))
            .build();

        assert_eq!(tool.name, "think");
        assert_eq!(tool.parameters["required"].as_array().unwrap().len(), 2);
        assert!(tool.parameters["properties"]["confidence"].is_object());
    }

    #[test]
    fn minimal_preset() {
        let tool = minimal_reasoning("reason");
        assert_eq!(tool.name, "reason");
        assert_eq!(tool.parameters["required"].as_array().unwrap().len(), 3);
    }

    #[test]
    fn routed_preset() {
        let tool = routed_reasoning("think", &["search", "edit"], &["safe", "blocked"]);
        assert_eq!(
            tool.parameters["properties"]["task_type"]["enum"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
    }
}
