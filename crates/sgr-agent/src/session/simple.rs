//! Default message types for agents that don't need custom role/message structs.
//!
//! Eliminates ~70 lines of boilerplate per agent. Use directly or as a reference
//! for custom implementations.

use super::traits::{AgentMessage, MessageRole};

/// Default 4-role enum implementing [`MessageRole`].
#[derive(Debug, Clone, PartialEq)]
pub enum SimpleRole {
    System,
    User,
    Assistant,
    Tool,
}

impl MessageRole for SimpleRole {
    fn system() -> Self {
        Self::System
    }
    fn user() -> Self {
        Self::User
    }
    fn assistant() -> Self {
        Self::Assistant
    }
    fn tool() -> Self {
        Self::Tool
    }

    fn as_str(&self) -> &str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
        }
    }

    fn parse_role(s: &str) -> Option<Self> {
        match s {
            "system" => Some(Self::System),
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }
}

/// Default message type implementing [`AgentMessage`].
#[derive(Debug, Clone)]
pub struct SimpleMsg {
    pub role: SimpleRole,
    pub content: String,
}

impl AgentMessage for SimpleMsg {
    type Role = SimpleRole;

    fn new(role: SimpleRole, content: String) -> Self {
        Self { role, content }
    }

    fn role(&self) -> &SimpleRole {
        &self.role
    }

    fn content(&self) -> &str {
        &self.content
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_roundtrip() {
        for (s, expected) in [
            ("system", SimpleRole::System),
            ("user", SimpleRole::User),
            ("assistant", SimpleRole::Assistant),
            ("tool", SimpleRole::Tool),
        ] {
            assert_eq!(SimpleRole::parse_role(s), Some(expected.clone()));
            assert_eq!(expected.as_str(), s);
        }
    }

    #[test]
    fn role_parse_invalid() {
        assert_eq!(SimpleRole::parse_role("unknown"), None);
        assert_eq!(SimpleRole::parse_role(""), None);
    }

    #[test]
    fn role_trait_constructors() {
        assert_eq!(SimpleRole::system(), SimpleRole::System);
        assert_eq!(SimpleRole::user(), SimpleRole::User);
        assert_eq!(SimpleRole::assistant(), SimpleRole::Assistant);
        assert_eq!(SimpleRole::tool(), SimpleRole::Tool);
    }

    #[test]
    fn role_is_system() {
        assert!(SimpleRole::System.is_system());
        assert!(!SimpleRole::User.is_system());
    }

    #[test]
    fn role_is_tool() {
        assert!(SimpleRole::Tool.is_tool());
        assert!(!SimpleRole::Assistant.is_tool());
    }

    #[test]
    fn msg_new() {
        let msg = SimpleMsg::new(SimpleRole::User, "hello".into());
        assert_eq!(*msg.role(), SimpleRole::User);
        assert_eq!(msg.content(), "hello");
    }

    #[test]
    fn msg_system() {
        let msg = SimpleMsg::new(SimpleRole::system(), "sys prompt".into());
        assert_eq!(*msg.role(), SimpleRole::System);
        assert_eq!(msg.content(), "sys prompt");
    }

    #[test]
    fn msg_clone() {
        let msg = SimpleMsg::new(SimpleRole::Assistant, "response".into());
        let cloned = msg.clone();
        assert_eq!(cloned.content(), "response");
        assert_eq!(*cloned.role(), SimpleRole::Assistant);
    }
}
