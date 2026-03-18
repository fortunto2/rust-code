//! Shared provider configuration, auth, and proxy infrastructure.
//!
//! Handles API key resolution, subscription-based auth (Claude Keychain, Codex),
//! and CLI subprocess proxies — reusable across all BAML agents.

mod auth;
mod cli_proxy;
mod codex_proxy;
mod config;

pub use auth::{
    KNOWN_PROVIDERS, ProviderAuth, ProviderEntry, load_claude_keychain_token, provider_names,
    resolve_provider,
};
pub use cli_proxy::{CliProvider, start_cli_proxy};
pub use codex_proxy::{CodexAuth, start_codex_proxy};
pub use config::{UserConfig, load_config, save_config};
