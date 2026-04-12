//! sgr-agent-tools — reusable file-system tools for sgr-agent based AI agents.
//!
//! 11 universal tools parameterized by `FileBackend` trait:
//!
//! | # | Tool | Category |
//! |---|------|----------|
//! | 1 | ReadTool | observe |
//! | 2 | WriteTool | act |
//! | 3 | DeleteTool | act |
//! | 4 | SearchTool | observe |
//! | 5 | ListTool | observe |
//! | 6 | TreeTool | observe |
//! | 7 | EvalTool | compute (feature "eval") |
//! | 8 | ReadAllTool | observe (batch) |
//! | 9 | MkDirTool | act (deferred) |
//! | 10 | MoveTool | act (deferred) |
//! | 11 | FindTool | observe (deferred) |
//!
//! # Usage
//!
//! ```rust,ignore
//! use sgr_agent_tools::{FileBackend, TreeTool, ReadTool, SearchTool, WriteTool};
//!
//! // Implement FileBackend for your runtime
//! impl FileBackend for MyBackend { ... }
//!
//! // Create tools
//! let b = Arc::new(MyBackend::new());
//! let registry = ToolRegistry::new()
//!     .register(ReadTool(b.clone()))
//!     .register(WriteTool(b.clone()))
//!     .register(SearchTool(b.clone()))
//!     .register(TreeTool(b.clone()))
//!     .register_deferred(MkDirTool(b.clone()));
//! ```

pub mod backend;
pub mod helpers;
pub mod trust;

// Core tools
pub mod delete;
pub mod list;
pub mod read;
pub mod read_all;
pub mod search;
pub mod tree;
pub mod write;

// Deferred tools
pub mod find;
pub mod mkdir;
pub mod move_file;

// Optional: eval (heavy dep on boa_engine)
#[cfg(feature = "eval")]
pub mod eval;

// Re-export the core trait
pub use backend::FileBackend;

// Re-export all tools
pub use delete::DeleteTool;
pub use find::FindTool;
pub use list::ListTool;
pub use mkdir::MkDirTool;
pub use move_file::MoveTool;
pub use read::ReadTool;
pub use read_all::ReadAllTool;
pub use search::SearchTool;
pub use tree::TreeTool;
pub use write::WriteTool;

#[cfg(feature = "eval")]
pub use eval::EvalTool;

// Re-export helpers for wrapper tools (PAC1 uses these for Pac1SearchTool, etc.)
pub use helpers::{backend_err, has_matches, unique_files_from_search};
pub use search::{auto_expand_search, expand_query, fuzzy_regex, is_regex, smart_search};
pub use trust::{infer_trust, wrap_with_meta};
