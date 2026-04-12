pub mod bash;
pub mod checkpoint;
pub mod cost;
pub mod delegate;
pub mod editor;
pub mod mcp;
pub mod search;
pub mod skills;
pub mod truncate;

// Tool trait implementations (one per tool or tool group)
pub mod api_tool;
pub mod apply_patch_tool;
pub mod bash_tool;
pub mod delegate_tools;
pub mod edit_file_tool;
pub mod editor_tool;
pub mod finish_tool;
pub mod git_tool;
pub mod mcp_tool;
pub mod memory_tool;
pub mod project_tools;
pub mod read_file_tool;
pub mod search_tool;
pub mod swarm_tools;
pub mod task_tool;
pub mod write_file_tool;

pub use bash::*;
pub use checkpoint::*;
pub use cost::*;
pub use editor::*;
pub use search::*;
pub use skills::*;
pub use truncate::truncate_output;

// Re-export shared tools from sgr-agent
pub use sgr_agent::app_tools::fs::{edit_file, read_file, write_file};
pub use sgr_agent::app_tools::git::{GitStatus, git_add, git_commit, git_diff, git_status};

// Re-export shared provider infrastructure from sgr-agent
pub use sgr_agent::providers::{
    CliProvider, CodexAuth, ProviderAuth, UserConfig, load_claude_keychain_token, load_config,
    save_config, start_cli_proxy, start_codex_proxy,
};
