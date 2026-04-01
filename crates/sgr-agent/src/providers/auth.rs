//! Provider authentication: resolution, keychain, env vars.

/// How a provider authenticates.
pub enum ProviderAuth {
    /// API key from environment variable.
    EnvKey(&'static str),
    /// Claude OAuth token from macOS Keychain.
    ClaudeKeychain,
    /// ChatGPT subscription via Codex Responses API proxy.
    CodexProxy,
    /// CLI subprocess proxy (claude, gemini, codex).
    CliProxy(&'static str),
    /// No auth needed (local models).
    None,
}

/// A provider entry: name → (BAML client name, auth method).
pub struct ProviderEntry {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub baml_client: &'static str,
    pub auth: fn() -> ProviderAuth,
}

/// Default provider registry — covers common providers.
/// Each agent may use different BAML client names; this provides sensible defaults.
pub static KNOWN_PROVIDERS: &[ProviderEntry] = &[
    ProviderEntry {
        name: "gemini",
        aliases: &[],
        baml_client: "Gemini31Pro",
        auth: || ProviderAuth::EnvKey("GEMINI_API_KEY"),
    },
    ProviderEntry {
        name: "vertex",
        aliases: &["vertex-ai"],
        baml_client: "VertexGemini",
        auth: || ProviderAuth::EnvKey("GOOGLE_APPLICATION_CREDENTIALS"),
    },
    ProviderEntry {
        name: "openai",
        aliases: &[],
        baml_client: "OpenAI",
        auth: || ProviderAuth::EnvKey("OPENAI_API_KEY"),
    },
    ProviderEntry {
        name: "ollama",
        aliases: &["local"],
        baml_client: "OllamaDefault",
        auth: || ProviderAuth::None,
    },
    ProviderEntry {
        name: "claude",
        aliases: &[],
        baml_client: "Claude",
        auth: || ProviderAuth::ClaudeKeychain,
    },
    ProviderEntry {
        name: "codex",
        aliases: &["chatgpt"],
        baml_client: "CodexProxy",
        auth: || ProviderAuth::CodexProxy,
    },
    ProviderEntry {
        name: "claude-cli",
        aliases: &[],
        baml_client: "CliProxy",
        auth: || ProviderAuth::CliProxy("claude"),
    },
    ProviderEntry {
        name: "gemini-cli",
        aliases: &[],
        baml_client: "CliProxy",
        auth: || ProviderAuth::CliProxy("gemini"),
    },
    ProviderEntry {
        name: "codex-cli",
        aliases: &[],
        baml_client: "CliProxy",
        auth: || ProviderAuth::CliProxy("codex"),
    },
    ProviderEntry {
        name: "anthropic",
        aliases: &[],
        baml_client: "Claude",
        auth: || ProviderAuth::EnvKey("ANTHROPIC_API_KEY"),
    },
];

/// Resolve provider name to (BAML client name, auth method).
/// Searches both primary names and aliases.
pub fn resolve_provider(name: &str) -> Option<(&'static str, ProviderAuth)> {
    let lower = name.to_lowercase();
    for entry in KNOWN_PROVIDERS {
        if entry.name == lower || entry.aliases.iter().any(|a| *a == lower) {
            return Some((entry.baml_client, (entry.auth)()));
        }
    }
    None
}

/// All known provider names for help text.
pub fn provider_names() -> Vec<&'static str> {
    KNOWN_PROVIDERS.iter().map(|e| e.name).collect()
}

/// Extract Claude OAuth token from macOS Keychain.
/// Token is stored by Claude Code CLI under "Claude Code-credentials".
// AI-NOTE: Token works as x-api-key for haiku only. Sonnet/opus get 429 (subscription tier limit).
// Token expires ~8h, needs refresh via console.anthropic.com/api/oauth/token. See CLAUDE_PROXY_RESEARCH.md.
pub fn load_claude_keychain_token() -> Result<String, String> {
    // First check env var (takes priority, like Claude Code does)
    if let Ok(token) = std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // Extract from macOS Keychain
    let output = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-s",
            "Claude Code-credentials",
            "-w",
        ])
        .output()
        .map_err(|e| format!("Failed to run `security` command: {}", e))?;

    if !output.status.success() {
        return Err(
            "No Claude credentials in Keychain. Run `claude` first to authenticate.".into(),
        );
    }

    let json_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let json: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("Invalid JSON from Keychain: {}", e))?;

    let token = json["claudeAiOauth"]["accessToken"]
        .as_str()
        .ok_or_else(|| "No accessToken in Keychain credentials".to_string())?;

    if token.is_empty() {
        return Err("Empty accessToken in Keychain".into());
    }

    Ok(token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_known_providers() {
        assert!(resolve_provider("gemini").is_some());
        assert!(resolve_provider("claude").is_some());
        assert!(resolve_provider("codex").is_some());
        assert!(resolve_provider("chatgpt").is_some()); // alias
        assert!(resolve_provider("local").is_some()); // alias
        assert!(resolve_provider("unknown").is_none());
    }

    #[test]
    fn provider_names_not_empty() {
        assert!(provider_names().len() >= 9);
    }
}
