//! Reusable tool implementations for SGR agents.
//!
//! These are the core tools any BAML agent needs: bash, filesystem, git.
//! Agent-specific tools (MCP, skills, editor) stay in the agent crate.

pub mod bash;
pub mod fs;
pub mod git;

pub use bash::{run_command, run_command_in, run_interactive, BashResult};
pub use fs::{edit_file, read_file, write_file};
pub use git::{git_add, git_commit, git_diff, git_status, GitStatus};
