//! Shared provider configuration, auth, and proxy infrastructure.
//!
//! Handles API key resolution, subscription-based auth (Claude Keychain, Codex),
//! and CLI subprocess proxies — reusable across all BAML agents.

mod auth;
mod cli_proxy;
mod codex_proxy;
mod config;

pub use auth::{
    load_claude_keychain_token, provider_names, resolve_provider, ProviderAuth, ProviderEntry,
    KNOWN_PROVIDERS,
};
pub use cli_proxy::{start_cli_proxy, CliProvider};
pub use codex_proxy::{start_codex_proxy, CodexAuth};
pub use config::{load_config, save_config, UserConfig};
