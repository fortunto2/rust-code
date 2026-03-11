//! User config: provider and model selection.
//!
//! Stored in `~/<agent_home>/config.toml` (e.g. `~/.rust-code/config.toml`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// User config stored in `~/<agent_home>/config.toml`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct UserConfig {
    /// Default provider: "gemini", "codex", "openai", "claude", "ollama"
    #[serde(default)]
    pub provider: Option<String>,
    /// Override model name (BAML client name like "OpenAI", "Gemini31Pro", etc.)
    #[serde(default)]
    pub model: Option<String>,
}

fn config_path(agent_home: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(agent_home).join("config.toml")
}

/// Load user config from `~/<agent_home>/config.toml`.
pub fn load_config(agent_home: &str) -> UserConfig {
    let path = config_path(agent_home);
    if !path.exists() {
        return UserConfig::default();
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return UserConfig::default(),
    };
    toml::from_str(&content).unwrap_or_default()
}

/// Save user config to `~/<agent_home>/config.toml`.
pub fn save_config(agent_home: &str, config: &UserConfig) -> Result<(), std::io::Error> {
    let path = config_path(agent_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    std::fs::write(&path, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_empty() {
        let cfg = UserConfig::default();
        assert!(cfg.provider.is_none());
        assert!(cfg.model.is_none());
    }

    #[test]
    fn roundtrip_toml() {
        let cfg = UserConfig {
            provider: Some("gemini".into()),
            model: Some("Gemini31Pro".into()),
        };
        let s = toml::to_string_pretty(&cfg).unwrap();
        let parsed: UserConfig = toml::from_str(&s).unwrap();
        assert_eq!(parsed.provider.as_deref(), Some("gemini"));
        assert_eq!(parsed.model.as_deref(), Some("Gemini31Pro"));
    }
}
