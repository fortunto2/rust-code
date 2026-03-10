//! Agent trait — decides what to do next given conversation history and tools.

use crate::types::{SgrError, ToolCall};

/// Agent's decision: what to do next.
#[derive(Debug, Clone)]
pub struct Decision {
    /// Agent's assessment of the current situation.
    pub situation: String,
    /// Task breakdown (reasoning steps).
    pub task: Vec<String>,
    /// Tool calls to execute.
    pub tool_calls: Vec<ToolCall>,
    /// If true, the agent considers the task done.
    pub completed: bool,
}

/// Errors from agent operations.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("LLM error: {0}")]
    Llm(#[from] SgrError),
    #[error("tool error: {0}")]
    Tool(String),
    #[error("loop detected after {0} iterations")]
    LoopDetected(usize),
    #[error("max steps reached: {0}")]
    MaxSteps(usize),
    #[error("cancelled")]
    Cancelled,
}

/// An agent that decides what tools to call given conversation history.
///
/// Lifecycle hooks (all have default no-op implementations):
/// - `prepare_context` — called before each step to modify context
/// - `prepare_tools` — called before each step to filter/modify tool set
/// - `after_action` — called after tool execution with results
#[async_trait::async_trait]
pub trait Agent: Send + Sync {
    /// Given messages and available tools, decide what to do next.
    async fn decide(
        &self,
        messages: &[crate::types::Message],
        tools: &crate::registry::ToolRegistry,
    ) -> Result<Decision, AgentError>;

    /// Hook: modify context before each step. Default: no-op.
    fn prepare_context(
        &self,
        _ctx: &mut crate::context::AgentContext,
        _messages: &[crate::types::Message],
    ) {
    }

    /// Hook: filter or reorder tools before each step.
    /// Returns tool names to include. Default: all tools.
    fn prepare_tools(
        &self,
        _ctx: &crate::context::AgentContext,
        tools: &crate::registry::ToolRegistry,
    ) -> Vec<String> {
        tools.list().iter().map(|t| t.name().to_string()).collect()
    }

    /// Hook: called after tool execution with the tool name and output.
    /// Can modify context or messages. Default: no-op.
    fn after_action(
        &self,
        _ctx: &mut crate::context::AgentContext,
        _tool_name: &str,
        _output: &str,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_completed() {
        let d = Decision {
            situation: "done".into(),
            task: vec![],
            tool_calls: vec![],
            completed: true,
        };
        assert!(d.completed);
    }

    #[test]
    fn agent_error_display() {
        let err = AgentError::LoopDetected(5);
        assert_eq!(err.to_string(), "loop detected after 5 iterations");
        let err = AgentError::MaxSteps(50);
        assert_eq!(err.to_string(), "max steps reached: 50");
    }
}
