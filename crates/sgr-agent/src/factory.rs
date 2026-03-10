//! AgentFactory — create agents from configuration.
//!
//! Allows defining agent type, system prompt, and options in a config Value,
//! then instantiating the right agent variant at runtime.

use crate::agent::Agent;
use crate::client::LlmClient;
use serde_json::Value;

/// Agent type selector.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentType {
    /// Structured output via union schema.
    Sgr,
    /// Native function calling.
    ToolCalling,
    /// Text-based flexible parsing.
    Flexible,
    /// 2-phase hybrid (reasoning + action).
    Hybrid,
    /// Read-only planning (wraps Sgr by default).
    Planning,
}

impl AgentType {
    /// Parse from string (case-insensitive).
    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "sgr" | "structured" => Some(Self::Sgr),
            "tool_calling" | "toolcalling" | "fc" | "function_calling" => Some(Self::ToolCalling),
            "flexible" | "text" | "iron" => Some(Self::Flexible),
            "hybrid" | "sgr_tool_calling" => Some(Self::Hybrid),
            "planning" | "plan" | "read_only" => Some(Self::Planning),
            _ => None,
        }
    }
}

/// Configuration for creating an agent.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub agent_type: AgentType,
    pub system_prompt: String,
    /// Max retry attempts for FlexibleAgent (default: 1, no retry).
    pub max_retries: usize,
    /// Max reasoning tools for HybridAgent (default: 1 — just ReasoningTool).
    pub max_reasoning_tools: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            agent_type: AgentType::Sgr,
            system_prompt: String::new(),
            max_retries: 1,
            max_reasoning_tools: 1,
        }
    }
}

impl AgentConfig {
    /// Parse from a JSON Value.
    ///
    /// Expected format:
    /// ```json
    /// {
    ///   "type": "sgr",
    ///   "system_prompt": "You are a coding agent.",
    ///   "max_retries": 3,
    ///   "max_reasoning_tools": 1
    /// }
    /// ```
    pub fn from_value(val: &Value) -> Result<Self, String> {
        let agent_type = val
            .get("type")
            .and_then(|t| t.as_str())
            .and_then(AgentType::from_str_loose)
            .unwrap_or(AgentType::Sgr);

        let system_prompt = val
            .get("system_prompt")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();

        let max_retries = val
            .get("max_retries")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;

        let max_reasoning_tools = val
            .get("max_reasoning_tools")
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;

        Ok(Self {
            agent_type,
            system_prompt,
            max_retries,
            max_reasoning_tools,
        })
    }
}

/// Create an agent from config + LLM client.
///
/// Returns a boxed `dyn Agent` to erase the concrete type.
pub fn create_agent<C: LlmClient + Clone + 'static>(
    config: &AgentConfig,
    client: C,
) -> Box<dyn Agent> {
    match config.agent_type {
        AgentType::Sgr => Box::new(crate::agents::sgr::SgrAgent::new(
            client,
            &config.system_prompt,
        )),
        AgentType::ToolCalling => Box::new(crate::agents::tool_calling::ToolCallingAgent::new(
            client,
            &config.system_prompt,
        )),
        AgentType::Flexible => Box::new(crate::agents::flexible::FlexibleAgent::new(
            client,
            &config.system_prompt,
            config.max_retries,
        )),
        AgentType::Hybrid => Box::new(crate::agents::hybrid::HybridAgent::new(
            client,
            &config.system_prompt,
        )),
        AgentType::Planning => {
            let inner = Box::new(crate::agents::sgr::SgrAgent::new(
                client,
                &config.system_prompt,
            ));
            Box::new(crate::agents::planning::PlanningAgent::new(inner))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_type_from_str() {
        assert_eq!(AgentType::from_str_loose("sgr"), Some(AgentType::Sgr));
        assert_eq!(
            AgentType::from_str_loose("structured"),
            Some(AgentType::Sgr)
        );
        assert_eq!(
            AgentType::from_str_loose("tool_calling"),
            Some(AgentType::ToolCalling)
        );
        assert_eq!(AgentType::from_str_loose("fc"), Some(AgentType::ToolCalling));
        assert_eq!(
            AgentType::from_str_loose("flexible"),
            Some(AgentType::Flexible)
        );
        assert_eq!(AgentType::from_str_loose("text"), Some(AgentType::Flexible));
        assert_eq!(
            AgentType::from_str_loose("hybrid"),
            Some(AgentType::Hybrid)
        );
        assert_eq!(AgentType::from_str_loose("unknown"), None);
    }

    #[test]
    fn config_from_value() {
        let val = json!({
            "type": "flexible",
            "system_prompt": "You are a test agent.",
            "max_retries": 5
        });
        let config = AgentConfig::from_value(&val).unwrap();
        assert_eq!(config.agent_type, AgentType::Flexible);
        assert_eq!(config.system_prompt, "You are a test agent.");
        assert_eq!(config.max_retries, 5);
    }

    #[test]
    fn config_defaults() {
        let val = json!({});
        let config = AgentConfig::from_value(&val).unwrap();
        assert_eq!(config.agent_type, AgentType::Sgr);
        assert!(config.system_prompt.is_empty());
        assert_eq!(config.max_retries, 1);
    }

    #[test]
    fn config_default_struct() {
        let config = AgentConfig::default();
        assert_eq!(config.agent_type, AgentType::Sgr);
        assert_eq!(config.max_retries, 1);
    }
}
