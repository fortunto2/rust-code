//! FileBackend trait — the abstraction over filesystem operations.
//!
//! Implement this for your runtime:
//! - `PcmClient` (BitGN Connect-RPC) for PAC1 agents
//! - `LocalFs` (std::fs) for CLI tools
//! - `MockFs` (HashMap) for tests

use anyhow::Result;

/// Filesystem backend that tools delegate to.
///
/// All paths are workspace-relative (e.g. "contacts/john.md", not absolute).
/// Implementations handle the actual I/O (RPC, local fs, in-memory mock).
#[async_trait::async_trait]
pub trait FileBackend: Send + Sync {
    /// Read file contents, optionally with line numbers and range.
    ///
    /// - `number`: if true, prefix each line with its 1-indexed number (like `cat -n`)
    /// - `start_line`/`end_line`: 1-indexed range (0 = no limit)
    async fn read(
        &self,
        path: &str,
        number: bool,
        start_line: i32,
        end_line: i32,
    ) -> Result<String>;

    /// Write content to a file, optionally replacing a line range.
    ///
    /// - `start_line`/`end_line`: 1-indexed range to replace (0 = overwrite entire file)
    async fn write(&self, path: &str, content: &str, start_line: i32, end_line: i32) -> Result<()>;

    /// Delete a file.
    async fn delete(&self, path: &str) -> Result<()>;

    /// Search file contents with regex pattern.
    ///
    /// Returns grep-style output: `path:line_number:content`
    async fn search(&self, root: &str, pattern: &str, limit: i32) -> Result<String>;

    /// List directory entries.
    ///
    /// Returns one entry per line, directories suffixed with `/`.
    async fn list(&self, path: &str) -> Result<String>;

    /// Show recursive directory tree.
    ///
    /// - `level`: max depth (0 = unlimited)
    async fn tree(&self, root: &str, level: i32) -> Result<String>;

    /// Get workspace context (date, time, environment info).
    async fn context(&self) -> Result<String>;

    /// Create a directory.
    async fn mkdir(&self, path: &str) -> Result<()>;

    /// Move or rename a file.
    async fn move_file(&self, from: &str, to: &str) -> Result<()>;

    /// Find files by name pattern.
    ///
    /// - `file_type`: "files", "dirs", or empty for all
    /// - `limit`: max results (0 = no limit)
    async fn find(&self, root: &str, name: &str, file_type: &str, limit: i32) -> Result<String>;
}
