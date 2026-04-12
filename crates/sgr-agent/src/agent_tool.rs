//! Tool trait — re-exported from sgr-agent-core.
//!
//! Implement `Tool` for each capability you want to expose to the agent.
//! Arguments arrive as `serde_json::Value`; use `parse_args` helper for typed deserialization.

pub use sgr_agent_core::agent_tool::{ContextModifier, Tool, ToolError, ToolOutput, parse_args};

// Re-export here for backwards compat (originally in agent_tool, moved to context in core)
pub use sgr_agent_core::context::MAX_TOKENS_OVERRIDE_KEY;
