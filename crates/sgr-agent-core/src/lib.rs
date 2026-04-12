//! sgr-agent-core — minimal core types for sgr-agent ecosystem.
//!
//! Contains: Tool trait, FileBackend trait, ToolDef, AgentContext, JSON Schema helpers.
//! No heavy deps — just serde, schemars, async-trait, thiserror, anyhow.

pub mod agent_tool;
pub mod backend;
pub mod context;
pub mod schema;
pub mod tool;

// Re-exports for convenience
pub use agent_tool::{ContextModifier, Tool, ToolError, ToolOutput, parse_args};
pub use backend::FileBackend;
pub use context::{AgentContext, AgentState, MAX_TOKENS_OVERRIDE_KEY};
pub use schema::{json_schema_for, make_openai_strict, response_schema_for, to_gemini_parameters};
pub use tool::{ToolDef, tool};
