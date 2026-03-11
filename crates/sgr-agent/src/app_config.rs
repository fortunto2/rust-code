use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentConfigError {
    #[error("Missing env var: {0}")]
    MissingEnvVar(String),
    #[error("Provider not found: {0}")]
    ProviderNotFound(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub provider_type: String, // "vertex-ai", "google-ai", "openai-generic"
    pub model: String,
    pub api_key_env_var: Option<String>,
    pub base_url: Option<String>,
    pub location: Option<String>,
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub default_provider: String,
    pub providers: HashMap<String, ProviderConfig>,
}

impl AgentConfig {
    /// Create config with Vertex AI defaults from environment.
    ///
    /// Reads `GOOGLE_CLOUD_PROJECT` env var. Returns error if missing.
    pub fn vertex_from_env() -> Result<Self, AgentConfigError> {
        let project_id = std::env::var("GOOGLE_CLOUD_PROJECT")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .ok_or_else(|| AgentConfigError::MissingEnvVar("GOOGLE_CLOUD_PROJECT".into()))?;

        let mut providers = HashMap::new();

        providers.insert(
            "vertex".into(),
            ProviderConfig {
                provider_type: "vertex-ai".into(),
                model: "gemini-3.1-flash-lite-preview".into(),
                api_key_env_var: None,
                base_url: None,
                location: Some("global".into()),
                project_id: Some(project_id.clone()),
            },
        );

        providers.insert(
            "vertex_fallback".into(),
            ProviderConfig {
                provider_type: "vertex-ai".into(),
                model: "gemini-3-flash-preview".into(),
                api_key_env_var: None,
                base_url: None,
                location: Some("global".into()),
                project_id: Some(project_id),
            },
        );

        providers.insert(
            "local".into(),
            ProviderConfig {
                provider_type: "openai-generic".into(),
                model: "llama3.2".into(),
                api_key_env_var: None,
                base_url: Some("http://localhost:11434/v1".into()),
                location: None,
                project_id: None,
            },
        );

        Ok(Self {
            default_provider: "vertex".into(),
            providers,
        })
    }

    /// Add or replace a provider.
    pub fn add_provider(&mut self, name: impl Into<String>, config: ProviderConfig) {
        self.providers.insert(name.into(), config);
    }

    /// Set Vertex project_id on all vertex-ai providers.
    pub fn set_vertex_project(&mut self, project_id: &str) {
        for p in self.providers.values_mut() {
            if p.provider_type == "vertex-ai" {
                p.project_id = Some(project_id.into());
                if p.location.is_none() {
                    p.location = Some("global".into());
                }
            }
        }
    }
}
