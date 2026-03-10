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
#[async_trait::async_trait]
pub trait Agent: Send + Sync {
    /// Given messages and available tools, decide what to do next.
    async fn decide(
        &self,
        messages: &[crate::types::Message],
        tools: &crate::registry::ToolRegistry,
    ) -> Result<Decision, AgentError>;
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
