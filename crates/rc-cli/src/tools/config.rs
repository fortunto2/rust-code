use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// User config stored in ~/.rust-code/config.toml
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UserConfig {
    /// Default provider: "gemini", "codex", "openai", "claude", "ollama"
    #[serde(default)]
    pub provider: Option<String>,
    /// Override model name (BAML client name like "OpenAI", "Gemini31Pro", etc.)
    #[serde(default)]
    pub model: Option<String>,
}

fn config_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".rust-code/config.toml")
}

pub fn load_config() -> UserConfig {
    let path = config_path();
    if !path.exists() {
        return UserConfig::default();
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return UserConfig::default(),
    };
    toml::from_str(&content).unwrap_or_default()
}

pub fn save_config(config: &UserConfig) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&path, content)?;
    Ok(())
}

/// Resolve provider name to (BAML client name, auth method).
/// Returns None if provider is unknown.
pub fn resolve_provider(provider: &str) -> Option<(&'static str, ProviderAuth)> {
    match provider.to_lowercase().as_str() {
        // Direct API providers (need API key in env)
        "gemini" => Some(("Gemini31Pro", ProviderAuth::EnvKey("GEMINI_API_KEY"))),
        "openai" => Some(("OpenAI", ProviderAuth::EnvKey("OPENAI_API_KEY"))),
        "ollama" | "local" => Some(("OllamaDefault", ProviderAuth::None)),
        // Claude: OAuth token from macOS Keychain → direct Anthropic API
        "claude" => Some(("Claude", ProviderAuth::ClaudeKeychain)),
        // Codex: ChatGPT subscription → Codex Responses API proxy
        "codex" | "chatgpt" => Some(("CodexProxy", ProviderAuth::CodexProxy)),
        // CLI subprocess providers (slower, fallback)
        "claude-cli" => Some(("CliProxy", ProviderAuth::CliProxy("claude"))),
        "gemini-cli" => Some(("CliProxy", ProviderAuth::CliProxy("gemini"))),
        "codex-cli" => Some(("CliProxy", ProviderAuth::CliProxy("codex"))),
        // Direct Anthropic API (with explicit key)
        "anthropic" => Some(("Claude", ProviderAuth::EnvKey("ANTHROPIC_API_KEY"))),
        _ => None,
    }
}

/// All known provider names for help text.
pub const PROVIDERS: &[&str] = &[
    "gemini", "claude", "codex", "openai", "anthropic", "ollama",
    "gemini-cli", "codex-cli", "claude-cli",
];

pub enum ProviderAuth {
    EnvKey(&'static str),
    ClaudeKeychain,
    CodexProxy,
    CliProxy(&'static str),
    None,
}

/// Extract Claude OAuth token from macOS Keychain.
/// Token is stored by Claude Code CLI under "Claude Code-credentials".
pub fn load_claude_keychain_token() -> anyhow::Result<String> {
    // First check env var (takes priority, like Claude Code does)
    if let Ok(token) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // Extract from macOS Keychain
    let output = std::process::Command::new("security")
        .args(["find-generic-password", "-s", "Claude Code-credentials", "-w"])
        .output()
        .map_err(|e| anyhow::anyhow!("Failed to run `security` command: {}", e))?;

    if !output.status.success() {
        return Err(anyhow::anyhow!(
            "No Claude credentials in Keychain. Run `claude` first to authenticate."
        ));
    }

    let json_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let json: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| anyhow::anyhow!("Invalid JSON from Keychain: {}", e))?;

    let token = json["claudeAiOauth"]["accessToken"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No accessToken in Keychain credentials"))?;

    if token.is_empty() {
        return Err(anyhow::anyhow!("Empty accessToken in Keychain"));
    }

    Ok(token.to_string())
}
