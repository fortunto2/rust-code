//! sgr-agent-tools — 15 reusable file-system tools for sgr-agent based AI agents.
//!
//! All tools are generic over [`FileBackend`] trait.
//!
//! **Core (11):** ReadTool (+ indentation mode), WriteTool (JSON repair),
//! DeleteTool (batch), SearchTool (smart search), ListTool, TreeTool,
//! ReadAllTool, CopyTool, MkDirTool, MoveTool, FindTool.
//!
//! **Optional:** EvalTool (feature `eval`), ShellTool (feature `shell`),
//! ApplyPatchTool (feature `patch` — Codex-compatible diff DSL).
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

// Dynamic context injection for skills
pub mod skill_context;

// Plan checklist tool (Codex-compatible update_plan)
pub mod plan;

// Deferred tools
pub mod copy;
pub mod find;
pub mod mkdir;
pub mod move_file;
pub mod prepend;

// Optional: eval (heavy dep on boa_engine)
#[cfg(feature = "eval")]
pub mod eval;

// Optional: shell (needs tokio process feature)
#[cfg(feature = "shell")]
pub mod shell;

// Optional: apply_patch (Codex-compatible diff editing)
#[cfg(feature = "patch")]
pub mod apply_patch;

// Optional: local filesystem backend (std::fs + tokio::fs)
#[cfg(feature = "local-fs")]
pub mod local_fs;

// In-memory mock backend (always available, zero deps)
pub mod mock_fs;

// Re-export the core trait
pub use backend::FileBackend;

#[cfg(feature = "local-fs")]
pub use local_fs::LocalFs;
pub use mock_fs::MockFs;

// Re-export all tools
pub use copy::CopyTool;
pub use delete::DeleteTool;
pub use find::FindTool;
pub use list::ListTool;
pub use mkdir::MkDirTool;
pub use move_file::MoveTool;
pub use prepend::PrependTool;
pub use read::ReadTool;
pub use read_all::ReadAllTool;
pub use search::SearchTool;
pub use tree::TreeTool;
pub use write::WriteTool;

pub use plan::{PlanState, PlanStep, UpdatePlanTool};

#[cfg(feature = "eval")]
pub use eval::EvalTool;

#[cfg(feature = "shell")]
pub use shell::ShellTool;

#[cfg(feature = "patch")]
pub use apply_patch::ApplyPatchTool;

// Re-export helpers for wrapper tools (PAC1 uses these for Pac1SearchTool, etc.)
pub use helpers::{backend_err, has_matches, truncate_output, unique_files_from_search};
pub use search::{auto_expand_search, expand_query, fuzzy_regex, is_regex, smart_search};
pub use trust::{infer_trust, wrap_with_meta};
