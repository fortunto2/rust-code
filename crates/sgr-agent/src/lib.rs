//! # sgr-agent — Schema-Guided Reasoning LLM client
//!
//! Pure Rust. No dlopen, no external binaries.
//! Works on iOS, Android, WASM — anywhere reqwest+rustls compiles.
//!
//! Two mechanisms combined:
//! - **Structured output** — response conforms to JSON Schema (SGR envelope)
//! - **Function calling** — tools as typed structs, model picks & fills params
//!
//! ## BAML as single source of truth
//!
//! `.baml` files define schemas once. Two backends consume them:
//! - `baml-cli generate` → `baml_client/` (macOS, dlopen runtime)
//! - `sgr-agent codegen` → Rust structs with `#[derive(JsonSchema)]` (iOS/Android, native HTTP)

pub mod baml_parser;
pub mod codegen;
pub mod schema;
pub mod tool;
pub mod types;

#[cfg(feature = "gemini")]
pub mod gemini;

#[cfg(feature = "openai")]
pub mod openai;

pub use schema::{json_schema_for, response_schema_for};
pub use tool::{tool, ToolDef};
pub use types::*;
