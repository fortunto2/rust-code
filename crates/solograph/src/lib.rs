//! Code intelligence: tree-sitter scanning, project maps, dependency parsing.
//!
//! Rust-native subset of SoloGraph functionality. For features requiring
//! embeddings or graph DB, use the SoloGraph MCP server.

pub mod deps;
pub mod repomap;
pub mod scanner;
pub mod symbols;

pub use deps::{Dependency, DependencyKind, parse_deps};
pub use repomap::{generate_context_map, generate_repomap};
pub use scanner::{FileInfo, ProjectStats, dir_tree, scan_project};
pub use symbols::{Symbol, SymbolKind, extract_symbols};
