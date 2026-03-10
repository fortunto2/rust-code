//! Agent variants — different strategies for LLM ↔ tool interaction.
//!
//! - `sgr` — structured output via discriminated union schema
//! - `tool_calling` — native function calling (Gemini FC / OpenAI tools)
//! - `flexible` — text-based with retry + error feedback
//! - `hybrid` — 2-phase: reasoning FC → action FC

pub mod flexible;
pub mod hybrid;
pub mod sgr;
pub mod tool_calling;
