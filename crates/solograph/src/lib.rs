//! Code intelligence: tree-sitter scanning, project maps, dependency parsing.
//!
//! Rust-native subset of SoloGraph functionality. For features requiring
//! embeddings or graph DB, use the SoloGraph MCP server.

pub mod deps;
pub mod repomap;
pub mod scanner;
pub mod symbols;

pub use deps::{parse_deps, Dependency, DependencyKind};
pub use repomap::{generate_context_map, generate_repomap};
pub use scanner::{dir_tree, scan_project, FileInfo, ProjectStats};
pub use symbols::{extract_symbols, Symbol, SymbolKind};
