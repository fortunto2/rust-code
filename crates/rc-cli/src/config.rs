//! Configuration layers: global (~/.rust-code/config.toml) → project (.rust-code/config.toml) → local (.rust-code/config.local.toml, gitignored) → env vars → CLI flags.

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

    /// Use Chat Completions API instead of Responses API.
    /// For compat endpoints: Cloudflare AI Gateway, OpenRouter compat, Workers AI.
    #[serde(default)]
    pub use_chat_api: bool,
}

impl Config {
    /// Load merged config from all layers.
    ///
    /// Priority (later overrides earlier):
    /// 1. Global: `~/.rust-code/config.toml`
    /// 2. Project: `.rust-code/config.toml` (shared, committed to git)
    /// 3. Local: `.rust-code/config.local.toml` (gitignored — API keys, personal overrides)
    /// 4. Environment variables (`RUST_CODE_MODEL`, `GEMINI_API_KEY`, etc.)
    pub fn load() -> Self {
        let mut config = Config::default();

        // Layer 1: global
        if let Some(home) = dirs::home_dir() {
            let global_path = home.join(".rust-code").join("config.toml");
            if let Some(layer) = Self::load_file(&global_path) {
                config.merge(layer);
            }
        }

        // Layer 2: project (shared, committed)
        let project_path = PathBuf::from(".rust-code").join("config.toml");
        if let Some(layer) = Self::load_file(&project_path) {
            config.merge(layer);
        }

        // Layer 3: local (gitignored — API keys, personal overrides)
        let local_path = PathBuf::from(".rust-code").join("config.local.toml");
        if let Some(layer) = Self::load_file(&local_path) {
            config.merge(layer);
        }

        // Layer 4: env vars
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
        if other.use_chat_api {
            self.use_chat_api = true;
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

        // Note: API keys and project IDs are NOT merged from env here.
        // genai auto-detects the correct key from env based on model name.
        // Vertex project ID is resolved by detect_gcloud_project() in to_llm_config().
    }

    /// Build LlmConfig from config layers.
    ///
    /// Priority: `model_override` (--model flag) → config model → env auto-detect.
    /// When `model` is set, genai auto-detects provider from name (gpt-* → OpenAI, etc.).
    /// When `provider` is set explicitly, it determines the default model.
    pub fn to_llm_config(&self, model_override: Option<String>) -> Option<sgr_agent::LlmConfig> {
        use sgr_agent::LlmConfig;

        let provider = self.provider.as_deref();

        // CLI subprocess backend (claude -p / gemini -p / codex exec)
        // Activated by provider = "claude-cli" or --model claude-cli
        if matches!(provider, Some("claude-cli" | "gemini-cli" | "codex-cli")) {
            let model = model_override
                .or_else(|| self.model.clone())
                .unwrap_or_else(|| provider.unwrap().to_string());
            return Some(LlmConfig::cli(model));
        }
        if let Some(ref m) = model_override {
            if sgr_agent::CliBackend::from_model(m).is_some() {
                return Some(LlmConfig::cli(m));
            }
        }

        // Vertex needs special handling (project_id + location)
        if provider == Some("vertex") {
            let project_id = self
                .project_id
                .clone()
                .or_else(|| detect_gcloud_project())?;
            let model = model_override
                .or_else(|| self.model.clone())
                .unwrap_or_else(|| "gemini-3.1-pro-preview".into());
            let location = self.location.clone().unwrap_or_else(|| "global".into());
            return Some(LlmConfig::vertex(project_id, model).location(location));
        }

        // Determine model: --model flag → config → provider default → env auto-detect
        let model = model_override.or_else(|| self.model.clone()).or_else(|| {
            match provider {
                Some("gemini" | "google") => Some("gemini-3.1-pro-preview".into()),
                Some("openai") => Some("gpt-4o".into()),
                Some("claude" | "anthropic") => Some("claude-sonnet-4-20250514".into()),
                // Local servers: the user knows what they loaded — pick a soft default.
                Some("ollama") => Some("llama3".into()),
                Some("mistralrs" | "mistral-rs") => Some("default".into()),
                Some("lmstudio" | "lm-studio") => Some("default".into()),
                Some("vllm") => Some("default".into()),
                Some("llamacpp" | "llama-cpp" | "llama.cpp") => Some("default".into()),
                // Hosted providers: model names change too often to hardcode (Llama 3.3 → 4,
                // DeepSeek-V3 → V4, etc.). User MUST set `model` in config. We auto-fill
                // base_url + token only; if model is missing the function returns None and
                // main prints the no-provider error.
                Some(
                    "groq" | "cerebras" | "deepinfra" | "together" | "fireworks" | "openrouter"
                    | "cloudflare" | "cloudflare-gateway",
                ) => None,
                _ => {
                    // No provider specified — detect from available env vars
                    if std::env::var("GEMINI_API_KEY").is_ok() {
                        Some("gemini-3.1-pro-preview".into())
                    } else if std::env::var("OPENAI_API_KEY").is_ok() {
                        Some("gpt-4o".into())
                    } else if std::env::var("ANTHROPIC_API_KEY").is_ok() {
                        Some("claude-sonnet-4-20250514".into())
                    } else if std::env::var("OPENROUTER_API_KEY").is_ok() {
                        Some("google/gemini-2.5-flash".into())
                    } else if std::env::var("CLOUDFLARE_ACCOUNT_ID").is_ok()
                        && (std::env::var("CLOUDFLARE_API_TOKEN").is_ok()
                            || std::env::var("CF_AI_API_KEY").is_ok())
                    {
                        Some("@cf/meta/llama-3.1-8b-instruct".into())
                    } else {
                        None
                    }
                }
            }
        });

        let model = model?;

        // OpenAI-compatible local/self-hosted backends: auto-fill base_url + Chat API.
        // base_url override from config still wins.
        let local_defaults: Option<&'static str> = match provider {
            Some("ollama") => Some("http://localhost:11434/v1"),
            Some("mistralrs" | "mistral-rs") => Some("http://localhost:1234/v1"),
            Some("lmstudio" | "lm-studio") => Some("http://localhost:1234/v1"),
            Some("vllm") => Some("http://localhost:8000/v1"),
            Some("llamacpp" | "llama-cpp" | "llama.cpp") => Some("http://localhost:8080/v1"),
            _ => None,
        };

        if let Some(default_url) = local_defaults {
            let url = self
                .base_url
                .clone()
                .unwrap_or_else(|| default_url.to_string());
            let key = self.api_key.clone().unwrap_or_else(|| "local".to_string());
            let mut cfg = LlmConfig::endpoint(key, url, &model);
            // Local servers all speak Chat Completions reliably; Responses support is
            // patchy (Ollama supports it, llama.cpp/vLLM/mistral.rs don't yet).
            cfg.use_chat_api = true;
            return Some(cfg);
        }

        // Hosted OpenAI-compatible inference providers — (base_url, env-var candidates).
        let hosted: Option<(&'static str, &'static [&'static str])> = match provider {
            Some("groq") => Some(("https://api.groq.com/openai/v1", &["GROQ_API_KEY"])),
            Some("cerebras") => Some(("https://api.cerebras.ai/v1", &["CEREBRAS_API_KEY"])),
            Some("deepinfra") => Some((
                "https://api.deepinfra.com/v1/openai",
                &["DEEPINFRA_TOKEN", "DEEPINFRA_API_KEY"],
            )),
            Some("together") => Some(("https://api.together.xyz/v1", &["TOGETHER_API_KEY"])),
            Some("fireworks") => Some((
                "https://api.fireworks.ai/inference/v1",
                &["FIREWORKS_API_KEY"],
            )),
            Some("openrouter") => Some(("https://openrouter.ai/api/v1", &["OPENROUTER_API_KEY"])),
            _ => None,
        };

        if let Some((default_url, env_keys)) = hosted {
            let token = self
                .api_key
                .clone()
                .or_else(|| env_keys.iter().find_map(|k| std::env::var(k).ok()))?;
            let url = self
                .base_url
                .clone()
                .unwrap_or_else(|| default_url.to_string());
            let mut cfg = LlmConfig::endpoint(token, url, &model);
            cfg.use_chat_api = true;
            return Some(cfg);
        }

        // Cloudflare Workers AI — direct REST endpoint, needs account id + token.
        if provider == Some("cloudflare") {
            let account = self
                .project_id
                .clone()
                .or_else(|| std::env::var("CLOUDFLARE_ACCOUNT_ID").ok())?;
            let token = self
                .api_key
                .clone()
                .or_else(|| std::env::var("CLOUDFLARE_API_TOKEN").ok())
                .or_else(|| std::env::var("CF_AI_API_KEY").ok())?;
            let url = self.base_url.clone().unwrap_or_else(|| {
                format!("https://api.cloudflare.com/client/v4/accounts/{account}/ai/v1")
            });
            let mut cfg = LlmConfig::endpoint(token, url, &model);
            cfg.use_chat_api = true;
            return Some(cfg);
        }

        // Cloudflare AI Gateway — multi-provider proxy with usage analytics.
        // URL pattern: gateway.ai.cloudflare.com/v1/{account}/{gateway}/compat
        if provider == Some("cloudflare-gateway") {
            let account = self
                .project_id
                .clone()
                .or_else(|| std::env::var("CLOUDFLARE_ACCOUNT_ID").ok())?;
            let gateway = self
                .location
                .clone()
                .or_else(|| std::env::var("CLOUDFLARE_GATEWAY").ok())
                .unwrap_or_else(|| "default".to_string());
            let token = self
                .api_key
                .clone()
                .or_else(|| std::env::var("CF_AI_API_KEY").ok())
                .or_else(|| std::env::var("CLOUDFLARE_API_TOKEN").ok())?;
            let url = self.base_url.clone().unwrap_or_else(|| {
                format!("https://gateway.ai.cloudflare.com/v1/{account}/{gateway}/compat")
            });
            let mut cfg = LlmConfig::endpoint(token, url, &model);
            cfg.use_chat_api = true;
            return Some(cfg);
        }

        // If explicit api_key in config file, use it
        if let Some(ref key) = self.api_key {
            let mut cfg = LlmConfig::with_key(key, &model);
            cfg.base_url = self.base_url.clone();
            cfg.use_chat_api = self.use_chat_api;
            return Some(cfg);
        }

        // Custom base_url from config
        if let Some(ref url) = self.base_url {
            let mut cfg = LlmConfig::endpoint("", url, &model);
            cfg.use_chat_api = self.use_chat_api;
            return Some(cfg);
        }

        // OpenRouter — needs explicit base_url
        if model.contains('/') {
            // Slash in model name = OpenRouter format (e.g. "google/gemini-2.5-flash")
            if let Ok(key) = std::env::var("OPENROUTER_API_KEY") {
                return Some(LlmConfig::endpoint(
                    key,
                    "https://openrouter.ai/api/v1",
                    &model,
                ));
            }
        }

        // Pure auto — genai detects provider from model name, uses env vars
        Some(LlmConfig::auto(&model))
    }
}

/// Detect GCP project from env or gcloud config.
fn detect_gcloud_project() -> Option<String> {
    if let Ok(p) = std::env::var("VERTEX_PROJECT") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    if let Ok(p) = std::env::var("GOOGLE_CLOUD_PROJECT") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    let home = std::env::var("HOME").ok()?;
    let adc_path =
        std::path::PathBuf::from(&home).join(".config/gcloud/application_default_credentials.json");
    if !adc_path.exists() {
        return None;
    }
    let output = std::process::Command::new("gcloud")
        .args(["config", "get-value", "project"])
        .output()
        .ok()?;
    if output.status.success() {
        let project = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !project.is_empty() && !project.contains("unset") {
            return Some(project);
        }
    }
    None
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

    #[test]
    fn local_layer_overrides_project() {
        // Simulates: project sets provider, local overrides api_key
        let mut config = Config {
            provider: Some("openai".into()),
            model: Some("gpt-4o".into()),
            ..Default::default()
        };
        // Local layer adds api_key without touching provider/model
        let local = Config {
            api_key: Some("sk-local-secret".into()),
            ..Default::default()
        };
        config.merge(local);
        assert_eq!(config.provider.as_deref(), Some("openai"));
        assert_eq!(config.model.as_deref(), Some("gpt-4o"));
        assert_eq!(config.api_key.as_deref(), Some("sk-local-secret"));
    }

    /// Issue #2: `provider = "ollama"` without explicit base_url must auto-fill the
    /// localhost endpoint and switch to Chat Completions API (Ollama doesn't speak Responses).
    #[test]
    fn ollama_provider_fills_localhost_and_chat_api() {
        let cfg = Config {
            provider: Some("ollama".into()),
            model: Some("7shi/borea-phi-3.5-coding:3.8b-mini-instruct-q6_K".into()),
            ..Default::default()
        };
        let llm = cfg.to_llm_config(None).expect("ollama config builds");
        assert_eq!(
            llm.base_url.as_deref(),
            Some("http://localhost:11434/v1"),
            "ollama must default to its localhost endpoint"
        );
        assert!(
            llm.use_chat_api,
            "ollama only speaks Chat Completions, not Responses"
        );
        assert_eq!(
            llm.model,
            "7shi/borea-phi-3.5-coding:3.8b-mini-instruct-q6_K"
        );
    }

    #[test]
    fn ollama_provider_with_custom_base_url_is_respected() {
        let cfg = Config {
            provider: Some("ollama".into()),
            model: Some("llama3".into()),
            base_url: Some("http://10.0.0.5:11434/v1".into()),
            ..Default::default()
        };
        let llm = cfg.to_llm_config(None).expect("ollama config builds");
        assert_eq!(llm.base_url.as_deref(), Some("http://10.0.0.5:11434/v1"));
        assert!(llm.use_chat_api);
    }

    #[test]
    fn mistralrs_provider_defaults_to_port_1234() {
        let cfg = Config {
            provider: Some("mistralrs".into()),
            model: Some("any-model".into()),
            ..Default::default()
        };
        let llm = cfg.to_llm_config(None).expect("mistralrs config builds");
        assert_eq!(llm.base_url.as_deref(), Some("http://localhost:1234/v1"));
        assert!(llm.use_chat_api);
    }

    #[test]
    fn local_provider_presets_use_known_ports() {
        // (provider, expected_default_url)
        let cases = [
            ("lmstudio", "http://localhost:1234/v1"),
            ("lm-studio", "http://localhost:1234/v1"),
            ("vllm", "http://localhost:8000/v1"),
            ("llamacpp", "http://localhost:8080/v1"),
            ("llama-cpp", "http://localhost:8080/v1"),
            ("llama.cpp", "http://localhost:8080/v1"),
            ("mistral-rs", "http://localhost:1234/v1"),
        ];
        for (provider, expected_url) in cases {
            let cfg = Config {
                provider: Some(provider.into()),
                model: Some("any".into()),
                ..Default::default()
            };
            let llm = cfg
                .to_llm_config(None)
                .unwrap_or_else(|| panic!("provider `{provider}` should build"));
            assert_eq!(
                llm.base_url.as_deref(),
                Some(expected_url),
                "provider `{provider}` base_url",
            );
            assert!(
                llm.use_chat_api,
                "provider `{provider}` must use Chat Completions",
            );
        }
    }

    /// Hosted providers must read their token from the documented env var
    /// AND build the canonical OpenAI-compatible endpoint.
    /// The user MUST set `model` — we don't hardcode names because they rotate.
    #[test]
    fn hosted_providers_read_env_var_and_build_endpoint() {
        let cases = [
            ("groq", "GROQ_API_KEY", "https://api.groq.com/openai/v1"),
            ("cerebras", "CEREBRAS_API_KEY", "https://api.cerebras.ai/v1"),
            (
                "deepinfra",
                "DEEPINFRA_TOKEN",
                "https://api.deepinfra.com/v1/openai",
            ),
            (
                "together",
                "TOGETHER_API_KEY",
                "https://api.together.xyz/v1",
            ),
            (
                "fireworks",
                "FIREWORKS_API_KEY",
                "https://api.fireworks.ai/inference/v1",
            ),
            (
                "openrouter",
                "OPENROUTER_API_KEY",
                "https://openrouter.ai/api/v1",
            ),
        ];
        for (provider, env_var, expected_url) in cases {
            // SAFETY: tests modify env in-process; cargo runs tests in this binary
            // serially unless `--test-threads` overrides, and we restore afterward.
            let saved = std::env::var(env_var).ok();
            unsafe {
                std::env::set_var(env_var, "test-token-1234");
            }

            let cfg = Config {
                provider: Some(provider.into()),
                model: Some("any-model".into()),
                ..Default::default()
            };
            let llm = cfg.to_llm_config(None);

            // Restore env before asserting (so a failure doesn't leak state).
            match saved {
                Some(v) => unsafe { std::env::set_var(env_var, v) },
                None => unsafe { std::env::remove_var(env_var) },
            }

            let llm = llm.unwrap_or_else(|| panic!("provider `{provider}` should build"));
            assert_eq!(
                llm.base_url.as_deref(),
                Some(expected_url),
                "provider `{provider}` base_url",
            );
            assert_eq!(
                llm.api_key.as_deref(),
                Some("test-token-1234"),
                "provider `{provider}` api_key picked up from {env_var}",
            );
            assert!(llm.use_chat_api, "provider `{provider}` use_chat_api");
        }
    }

    /// Hosted providers must NOT hardcode model names — model lineups change every
    /// few months (Llama 3.3 → 4, DeepSeek V3 → V4, …). When the user picks the
    /// provider but forgets to set `model`, fail clean instead of pointing at a
    /// model that may already be deprecated.
    #[test]
    fn hosted_providers_have_no_default_model() {
        for provider in [
            "groq",
            "cerebras",
            "deepinfra",
            "together",
            "fireworks",
            "openrouter",
            "cloudflare",
            "cloudflare-gateway",
        ] {
            let cfg = Config {
                provider: Some(provider.into()),
                model: None,
                api_key: Some("test-token".into()),
                project_id: Some("acct".into()),
                ..Default::default()
            };
            assert!(
                cfg.to_llm_config(None).is_none(),
                "provider `{provider}` must not build without an explicit model",
            );
        }
    }

    /// Cloudflare AI Gateway: URL pattern is
    /// `gateway.ai.cloudflare.com/v1/{account}/{gateway}/compat`.
    /// Account/gateway/token must be readable from config and from env.
    #[test]
    fn cloudflare_gateway_builds_compat_url() {
        let cfg = Config {
            provider: Some("cloudflare-gateway".into()),
            model: Some("@cf/meta/llama-3.1-8b-instruct".into()),
            api_key: Some("test-cf-token".into()),
            project_id: Some("abc123".into()),
            location: Some("mygw".into()),
            ..Default::default()
        };
        let llm = cfg.to_llm_config(None).expect("cf-gateway builds");
        assert_eq!(
            llm.base_url.as_deref(),
            Some("https://gateway.ai.cloudflare.com/v1/abc123/mygw/compat"),
        );
        assert_eq!(llm.api_key.as_deref(), Some("test-cf-token"));
        assert!(llm.use_chat_api);
    }

    #[test]
    fn cloudflare_workers_ai_builds_account_url() {
        let cfg = Config {
            provider: Some("cloudflare".into()),
            model: Some("@cf/meta/llama-3.1-8b-instruct".into()),
            api_key: Some("test-cf-token".into()),
            project_id: Some("abc123".into()),
            ..Default::default()
        };
        let llm = cfg.to_llm_config(None).expect("cf workers ai builds");
        assert_eq!(
            llm.base_url.as_deref(),
            Some("https://api.cloudflare.com/client/v4/accounts/abc123/ai/v1"),
        );
        assert_eq!(llm.api_key.as_deref(), Some("test-cf-token"));
        assert!(llm.use_chat_api);
    }

    #[test]
    fn three_layer_merge() {
        // Global
        let mut config = Config {
            provider: Some("gemini".into()),
            max_steps: Some(10),
            ..Default::default()
        };
        // Project overrides model
        config.merge(Config {
            model: Some("gpt-4o".into()),
            provider: Some("openai".into()),
            ..Default::default()
        });
        // Local adds api_key + overrides max_steps
        config.merge(Config {
            api_key: Some("sk-secret".into()),
            max_steps: Some(50),
            ..Default::default()
        });
        assert_eq!(config.provider.as_deref(), Some("openai")); // from project
        assert_eq!(config.model.as_deref(), Some("gpt-4o")); // from project
        assert_eq!(config.api_key.as_deref(), Some("sk-secret")); // from local
        assert_eq!(config.max_steps, Some(50)); // from local
    }
}
