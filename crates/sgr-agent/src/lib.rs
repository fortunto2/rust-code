//! # sgr-agent — LLM client + agent framework
//!
//! Pure Rust. No dlopen, no external binaries.
//! Works on iOS, Android, WASM — anywhere reqwest+rustls compiles.
//!
//! ## LLM Client (default)
//! - **Structured output** — response conforms to JSON Schema (SGR envelope)
//! - **Function calling** — tools as typed structs, model picks & fills params
//! - **Flexible parser** — extract JSON from markdown, broken JSON, streaming chunks
//! - **Backends**: Gemini (Google AI + Vertex AI), OpenAI (+ OpenRouter, Ollama)
//!
//! ## Agent Framework (`feature = "agent"`)
//! - **Tool trait** — define tools with typed args + async execute
//! - **ToolRegistry** — ordered collection, case-insensitive lookup, fuzzy resolve
//! - **Agent trait** — decides what tools to call given conversation history
//! - **3 agent variants**: SgrAgent (structured output), ToolCallingAgent (native FC), FlexibleAgent (text parse)
//! - **Agent loop** — decide → execute → feed back, with 3-tier loop detection
//! - **Progressive discovery** — filter tools by relevance (TF-IDF scoring)

pub mod baml_parser;
pub mod codegen;
pub mod coerce;
pub mod flexible_parser;
pub mod schema;
pub mod tool;
pub mod types;

#[cfg(feature = "gemini")]
pub mod gemini;

#[cfg(feature = "openai")]
pub mod openai;

// Agent framework (behind feature gate)
#[cfg(feature = "agent")]
pub mod agent;
#[cfg(feature = "agent")]
pub mod agent_loop;
#[cfg(feature = "agent")]
pub mod agent_tool;
#[cfg(feature = "agent")]
pub mod agents;
#[cfg(feature = "agent")]
pub mod client;
#[cfg(feature = "agent")]
pub mod context;
#[cfg(feature = "agent")]
pub mod discovery;
#[cfg(feature = "agent")]
pub mod factory;
#[cfg(feature = "agent")]
pub mod registry;
#[cfg(feature = "agent")]
pub mod retry;
#[cfg(feature = "agent")]
pub mod router;
#[cfg(feature = "agent")]
pub mod schema_simplifier;
#[cfg(feature = "agent")]
pub mod streaming;
#[cfg(feature = "agent")]
pub mod prompt_loader;
#[cfg(feature = "agent")]
pub mod swarm;
#[cfg(feature = "agent")]
pub mod swarm_tools;
#[cfg(feature = "agent")]
pub mod compaction;
#[cfg(feature = "agent")]
pub mod union_schema;

pub use coerce::coerce_value;
pub use flexible_parser::{parse_flexible, parse_flexible_coerced};
pub use schema::{json_schema_for, response_schema_for};
pub use tool::{tool, ToolDef};
pub use types::*;
