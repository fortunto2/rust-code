//! Core traits and types for the session system.

/// Role of a message in the agent conversation.
pub trait MessageRole: Clone + PartialEq {
    fn system() -> Self;
    fn user() -> Self;
    fn assistant() -> Self;
    fn tool() -> Self;
    fn as_str(&self) -> &str;
    fn parse_role(s: &str) -> Option<Self>;
    fn is_system(&self) -> bool {
        self.as_str() == "system"
    }
    fn is_tool(&self) -> bool {
        self.as_str() == "tool"
    }
}

/// A message in the agent conversation.
pub trait AgentMessage: Clone {
    type Role: MessageRole;
    fn new(role: Self::Role, content: String) -> Self;
    fn role(&self) -> &Self::Role;
    fn content(&self) -> &str;
    /// Attach a tool call ID for Responses API stateful chaining.
    /// Default: no-op (returns self unchanged).
    fn with_call_id(self, _call_id: String) -> Self {
        self
    }
}

/// Entry type discriminator — prevents invalid types at compile time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    User,
    Assistant,
    System,
    Tool,
}

impl EntryType {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "system" => Some(Self::System),
            "tool" => Some(Self::Tool),
            _ => None,
        }
    }

    pub(crate) fn into_role<R: MessageRole>(self) -> R {
        match self {
            Self::User => R::user(),
            Self::Assistant => R::assistant(),
            Self::System => R::system(),
            Self::Tool => R::tool(),
        }
    }
}
