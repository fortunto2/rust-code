//! Configuration layers: global (~/.rust-code/config.toml) → project (.rust-code/config.toml) → env vars → CLI flags.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Merged configuration from all layers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    /// Primary model name.
    #[serde(default)]
    pub model: Option<String>,

    /// Provider: "gemini", "vertex", "openai".
    #[serde(default)]
    pub provider: Option<String>,

    /// API key (prefer env var).
    #[serde(default)]
    pub api_key: Option<String>,

    /// Base URL override (for OpenAI-compatible).
    #[serde(default)]
    pub base_url: Option<String>,

    /// Vertex project ID.
    #[serde(default)]
    pub project_id: Option<String>,

    /// Vertex region.
    #[serde(default)]
    pub location: Option<String>,

    /// Max conversation history messages before trimming.
    #[serde(default)]
    pub max_history: Option<usize>,

    /// Compaction token threshold.
    #[serde(default)]
    pub compaction_threshold: Option<usize>,

    /// Max agent steps (headless mode).
    #[serde(default)]
    pub max_steps: Option<usize>,

    /// Model for compaction/summarization (cheap fast model).
    #[serde(default)]
    pub compaction_model: Option<String>,
}

impl Config {
    /// Load merged config from all layers.
    ///
    /// Priority (later overrides earlier):
    /// 1. Global: ~/.rust-code/config.toml
    /// 2. Project: .rust-code/config.toml
    /// 3. Environment variables (RUST_CODE_MODEL, GEMINI_API_KEY, etc.)
    pub fn load() -> Self {
        let mut config = Config::default();

        // Layer 1: global
        if let Some(home) = dirs::home_dir() {
            let global_path = home.join(".rust-code").join("config.toml");
            if let Some(layer) = Self::load_file(&global_path) {
                config.merge(layer);
            }
        }

        // Layer 2: project
        let project_path = PathBuf::from(".rust-code").join("config.toml");
        if let Some(layer) = Self::load_file(&project_path) {
            config.merge(layer);
        }

        // Layer 3: env vars
        config.merge_env();

        config
    }

    /// Load a single TOML config file.
    fn load_file(path: &Path) -> Option<Config> {
        let content = std::fs::read_to_string(path).ok()?;
        match toml::from_str(&content) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                tracing::warn!("Failed to parse {}: {}", path.display(), e);
                None
            }
        }
    }

    /// Merge another config layer — non-None values override.
    fn merge(&mut self, other: Config) {
        if other.model.is_some() {
            self.model = other.model;
        }
        if other.provider.is_some() {
            self.provider = other.provider;
        }
        if other.api_key.is_some() {
            self.api_key = other.api_key;
        }
        if other.base_url.is_some() {
            self.base_url = other.base_url;
        }
        if other.project_id.is_some() {
            self.project_id = other.project_id;
        }
        if other.location.is_some() {
            self.location = other.location;
        }
        if other.max_history.is_some() {
            self.max_history = other.max_history;
        }
        if other.compaction_threshold.is_some() {
            self.compaction_threshold = other.compaction_threshold;
        }
        if other.max_steps.is_some() {
            self.max_steps = other.max_steps;
        }
        if other.compaction_model.is_some() {
            self.compaction_model = other.compaction_model;
        }
    }

    /// Merge environment variables.
    fn merge_env(&mut self) {
        if let Ok(v) = std::env::var("RUST_CODE_MODEL") {
            self.model = Some(v);
        }
        if let Ok(v) = std::env::var("RUST_CODE_PROVIDER") {
            self.provider = Some(v);
        }
        if let Ok(v) = std::env::var("RUST_CODE_MAX_STEPS") {
            if let Ok(n) = v.parse() {
                self.max_steps = Some(n);
            }
        }

        // API key fallback chain
        if self.api_key.is_none() {
            if let Ok(v) = std::env::var("GEMINI_API_KEY") {
                self.api_key = Some(v);
            }
        }
        if self.api_key.is_none() {
            if let Ok(v) = std::env::var("OPENAI_API_KEY") {
                self.api_key = Some(v);
            }
        }
        if self.project_id.is_none() {
            if let Ok(v) = std::env::var("GOOGLE_CLOUD_PROJECT") {
                self.project_id = Some(v);
            }
        }
    }

    /// Resolve provider from config.
    /// Returns SgrProvider suitable for the agent.
    pub fn resolve_provider(&self) -> Option<crate::backend::SgrProvider> {
        let provider = self.provider.as_deref().unwrap_or("gemini");

        match provider {
            "gemini" => {
                let api_key = self.api_key.clone()?;
                let model = self
                    .model
                    .clone()
                    .unwrap_or_else(|| "gemini-2.5-flash".into());
                Some(crate::backend::SgrProvider::Gemini { api_key, model })
            }
            "vertex" => {
                let project_id = self.project_id.clone()?;
                let model = self
                    .model
                    .clone()
                    .unwrap_or_else(|| "gemini-2.5-flash".into());
                let location = self.location.clone().unwrap_or_else(|| "global".into());
                Some(crate::backend::SgrProvider::Vertex {
                    project_id,
                    model,
                    location,
                })
            }
            "openai" => {
                let api_key = self.api_key.clone()?;
                let model = self.model.clone().unwrap_or_else(|| "gpt-4o".into());
                Some(crate::backend::SgrProvider::OpenAI {
                    api_key,
                    model,
                    base_url: self.base_url.clone(),
                })
            }
            _ => {
                tracing::warn!("Unknown provider: {}", provider);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_overrides() {
        let mut base = Config {
            model: Some("base-model".into()),
            provider: Some("gemini".into()),
            ..Default::default()
        };
        let overlay = Config {
            model: Some("overlay-model".into()),
            max_steps: Some(100),
            ..Default::default()
        };
        base.merge(overlay);
        assert_eq!(base.model.as_deref(), Some("overlay-model"));
        assert_eq!(base.provider.as_deref(), Some("gemini")); // not overridden
        assert_eq!(base.max_steps, Some(100));
    }

    #[test]
    fn default_is_empty() {
        let cfg = Config::default();
        assert!(cfg.model.is_none());
        assert!(cfg.provider.is_none());
    }
}
