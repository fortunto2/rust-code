use crate::config::AgentConfig;
use std::collections::HashMap;

/// Trait abstracting BAML's generated `ClientRegistry`.
///
/// Each BAML project generates its own `ClientRegistry` type, but the API
/// is identical. Implement this trait on a newtype wrapper around your
/// project's `ClientRegistry`.
///
/// ```ignore
/// struct MyRegistry(baml_client::ClientRegistry);
///
/// impl BamlRegistry for MyRegistry {
///     fn new() -> Self { Self(baml_client::ClientRegistry::new()) }
///     fn add_llm_client(&mut self, name: &str, provider_type: &str, options: HashMap<String, serde_json::Value>) {
///         self.0.add_llm_client(name, provider_type, options);
///     }
///     fn set_primary_client(&mut self, name: &str) { self.0.set_primary_client(name); }
///     fn into_inner(self) -> baml_client::ClientRegistry { self.0 }
/// }
/// ```
pub trait BamlRegistry: Sized {
    fn new() -> Self;
    fn add_llm_client(
        &mut self,
        name: &str,
        provider_type: &str,
        options: HashMap<String, serde_json::Value>,
    );
    fn set_primary_client(&mut self, name: &str);
}

/// Generic engine that builds a `BamlRegistry` from `AgentConfig`.
pub struct AgentEngine {
    config: AgentConfig,
}

impl AgentEngine {
    pub fn new(config: AgentConfig) -> Self {
        Self { config }
    }

    /// Build a BAML ClientRegistry from the agent config.
    ///
    /// Iterates all providers, sets options (model, base_url, location,
    /// project_id, api_key), and sets the primary client.
    pub fn build_registry<R: BamlRegistry>(&self) -> Result<R, String> {
        if !self.config.providers.contains_key(&self.config.default_provider) {
            return Err(format!(
                "default provider '{}' is not configured",
                self.config.default_provider
            ));
        }

        let mut registry = R::new();

        for (name, conf) in &self.config.providers {
            let mut options: HashMap<String, serde_json::Value> = HashMap::new();
            options.insert("model".into(), serde_json::json!(conf.model));

            if let Some(url) = &conf.base_url {
                options.insert("base_url".into(), serde_json::json!(url));
            }
            if let Some(loc) = &conf.location {
                options.insert("location".into(), serde_json::json!(loc));
            }
            if let Some(pid) = &conf.project_id {
                options.insert("project_id".into(), serde_json::json!(pid));
            }
            if let Some(env_var) = &conf.api_key_env_var {
                options.insert("api_key".into(), serde_json::json!(format!("env.{}", env_var)));
            }

            registry.add_llm_client(name, &conf.provider_type, options);
        }

        registry.set_primary_client(&self.config.default_provider);
        Ok(registry)
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }
}
