//! Model router — routes requests to smart or fast model based on complexity.
//!
//! Wraps two `LlmClient` instances and selects which to use per call:
//! - **Smart model** (e.g. gemini-3.1-pro): for complex reasoning, many tools, long context
//! - **Fast model** (e.g. gemini-3.1-flash): for simple tool calls, short context
//!
//! Selection heuristics:
//! - Message count > threshold → smart
//! - Tool count > threshold → smart
//! - Schema complexity (deep nesting) → smart
//! - Otherwise → fast

use crate::client::LlmClient;
use crate::tool::ToolDef;
use crate::types::{Message, SgrError, ToolCall};
use serde_json::Value;

/// Configuration for the model router.
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Messages above this count route to smart model.
    pub message_threshold: usize,
    /// Tools above this count route to smart model.
    pub tool_threshold: usize,
    /// Always use smart model (bypass routing).
    pub always_smart: bool,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            message_threshold: 10,
            tool_threshold: 8,
            always_smart: false,
        }
    }
}

/// Which model was selected for a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelChoice {
    Smart,
    Fast,
}

/// Dual-model router that selects smart or fast model per request.
pub struct ModelRouter<S: LlmClient, F: LlmClient> {
    smart: S,
    fast: F,
    config: RouterConfig,
}

impl<S: LlmClient, F: LlmClient> ModelRouter<S, F> {
    pub fn new(smart: S, fast: F) -> Self {
        Self {
            smart,
            fast,
            config: RouterConfig::default(),
        }
    }

    pub fn with_config(mut self, config: RouterConfig) -> Self {
        self.config = config;
        self
    }

    /// Decide which model to use based on request characteristics.
    pub fn route_messages(&self, messages: &[Message]) -> ModelChoice {
        if self.config.always_smart {
            return ModelChoice::Smart;
        }
        if messages.len() > self.config.message_threshold {
            return ModelChoice::Smart;
        }
        ModelChoice::Fast
    }

    /// Decide which model for tool calls based on tool count.
    pub fn route_tools(&self, messages: &[Message], tools: &[ToolDef]) -> ModelChoice {
        if self.config.always_smart {
            return ModelChoice::Smart;
        }
        if messages.len() > self.config.message_threshold {
            return ModelChoice::Smart;
        }
        if tools.len() > self.config.tool_threshold {
            return ModelChoice::Smart;
        }
        ModelChoice::Fast
    }

    /// Decide which model for structured calls.
    pub fn route_structured(&self, messages: &[Message], _schema: &Value) -> ModelChoice {
        if self.config.always_smart {
            return ModelChoice::Smart;
        }
        // Structured output with many messages → smart
        if messages.len() > self.config.message_threshold {
            return ModelChoice::Smart;
        }
        // Structured calls are generally harder → smart for safety
        ModelChoice::Smart
    }
}

#[async_trait::async_trait]
impl<S: LlmClient, F: LlmClient> LlmClient for ModelRouter<S, F> {
    async fn structured_call(
        &self,
        messages: &[Message],
        schema: &Value,
    ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
        match self.route_structured(messages, schema) {
            ModelChoice::Smart => self.smart.structured_call(messages, schema).await,
            ModelChoice::Fast => self.fast.structured_call(messages, schema).await,
        }
    }

    async fn tools_call(
        &self,
        messages: &[Message],
        tools: &[ToolDef],
    ) -> Result<Vec<ToolCall>, SgrError> {
        match self.route_tools(messages, tools) {
            ModelChoice::Smart => self.smart.tools_call(messages, tools).await,
            ModelChoice::Fast => self.fast.tools_call(messages, tools).await,
        }
    }

    async fn complete(&self, messages: &[Message]) -> Result<String, SgrError> {
        match self.route_messages(messages) {
            ModelChoice::Smart => self.smart.complete(messages).await,
            ModelChoice::Fast => self.fast.complete(messages).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn router_config_defaults() {
        let config = RouterConfig::default();
        assert!(!config.always_smart);
        assert_eq!(config.message_threshold, 10);
        assert_eq!(config.tool_threshold, 8);
    }

    #[test]
    fn route_messages_logic() {
        struct DummyClient;
        #[async_trait::async_trait]
        impl LlmClient for DummyClient {
            async fn structured_call(
                &self,
                _: &[Message],
                _: &Value,
            ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
                Ok((None, vec![], String::new()))
            }
            async fn tools_call(
                &self,
                _: &[Message],
                _: &[ToolDef],
            ) -> Result<Vec<ToolCall>, SgrError> {
                Ok(vec![])
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                Ok(String::new())
            }
        }

        let router = ModelRouter::new(DummyClient, DummyClient);

        // Short conversation → fast
        let short: Vec<Message> = (0..3).map(|_| Message::user("hi")).collect();
        assert_eq!(router.route_messages(&short), ModelChoice::Fast);

        // Long conversation → smart
        let long: Vec<Message> = (0..15).map(|_| Message::user("hi")).collect();
        assert_eq!(router.route_messages(&long), ModelChoice::Smart);
    }

    #[test]
    fn route_tools_logic() {
        struct DummyClient;
        #[async_trait::async_trait]
        impl LlmClient for DummyClient {
            async fn structured_call(
                &self,
                _: &[Message],
                _: &Value,
            ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
                Ok((None, vec![], String::new()))
            }
            async fn tools_call(
                &self,
                _: &[Message],
                _: &[ToolDef],
            ) -> Result<Vec<ToolCall>, SgrError> {
                Ok(vec![])
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                Ok(String::new())
            }
        }

        let router = ModelRouter::new(DummyClient, DummyClient);
        let msgs = vec![Message::user("hi")];

        // Few tools → fast
        let few_tools: Vec<ToolDef> = (0..3)
            .map(|i| ToolDef {
                name: format!("tool_{}", i),
                description: "test".into(),
                parameters: serde_json::json!({}),
            })
            .collect();
        assert_eq!(router.route_tools(&msgs, &few_tools), ModelChoice::Fast);

        // Many tools → smart
        let many_tools: Vec<ToolDef> = (0..12)
            .map(|i| ToolDef {
                name: format!("tool_{}", i),
                description: "test".into(),
                parameters: serde_json::json!({}),
            })
            .collect();
        assert_eq!(router.route_tools(&msgs, &many_tools), ModelChoice::Smart);
    }

    #[test]
    fn always_smart_overrides() {
        struct DummyClient;
        #[async_trait::async_trait]
        impl LlmClient for DummyClient {
            async fn structured_call(
                &self,
                _: &[Message],
                _: &Value,
            ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
                Ok((None, vec![], String::new()))
            }
            async fn tools_call(
                &self,
                _: &[Message],
                _: &[ToolDef],
            ) -> Result<Vec<ToolCall>, SgrError> {
                Ok(vec![])
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                Ok(String::new())
            }
        }

        let router = ModelRouter::new(DummyClient, DummyClient).with_config(RouterConfig {
            always_smart: true,
            ..Default::default()
        });

        let msgs = vec![Message::user("hi")];
        assert_eq!(router.route_messages(&msgs), ModelChoice::Smart);
        assert_eq!(router.route_tools(&msgs, &[]), ModelChoice::Smart);
    }

    #[test]
    fn structured_defaults_to_smart() {
        struct DummyClient;
        #[async_trait::async_trait]
        impl LlmClient for DummyClient {
            async fn structured_call(
                &self,
                _: &[Message],
                _: &Value,
            ) -> Result<(Option<Value>, Vec<ToolCall>, String), SgrError> {
                Ok((None, vec![], String::new()))
            }
            async fn tools_call(
                &self,
                _: &[Message],
                _: &[ToolDef],
            ) -> Result<Vec<ToolCall>, SgrError> {
                Ok(vec![])
            }
            async fn complete(&self, _: &[Message]) -> Result<String, SgrError> {
                Ok(String::new())
            }
        }

        let router = ModelRouter::new(DummyClient, DummyClient);
        let msgs = vec![Message::user("hi")];
        // Structured calls always prefer smart
        assert_eq!(
            router.route_structured(&msgs, &serde_json::json!({})),
            ModelChoice::Smart
        );
    }
}
