//! Auto-discovery and caching of OpenAPI specs.
//!
//! - Hardcoded popular APIs (GitHub, Stripe, etc.) with known spec URLs
//! - APIs.guru directory as fallback (2800+ APIs)
//! - Local cache in `~/.sgr-agent/openapi-cache/`

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A known API spec source.
#[derive(Debug, Clone)]
pub struct ApiSpec {
    /// Short name: "github", "stripe"
    pub name: String,
    /// Human description
    pub description: String,
    /// URL to download the OpenAPI JSON spec
    pub spec_url: String,
    /// Default base URL for API calls
    pub base_url: String,
    /// Auth env var hint (e.g. "GITHUB_TOKEN")
    pub auth_env: Option<String>,
}

/// Popular APIs useful for development — hardcoded for instant access.
pub fn popular_apis() -> Vec<ApiSpec> {
    vec![
        ApiSpec {
            name: "github".into(),
            description: "GitHub REST API v3 — repos, issues, PRs, actions".into(),
            spec_url: "https://raw.githubusercontent.com/github/rest-api-description/main/descriptions/api.github.com/api.github.com.json".into(),
            base_url: "https://api.github.com".into(),
            auth_env: Some("GITHUB_TOKEN".into()),
        },
        ApiSpec {
            name: "stripe".into(),
            description: "Stripe API — payments, subscriptions, customers".into(),
            spec_url: "https://raw.githubusercontent.com/stripe/openapi/master/openapi/spec3.json".into(),
            base_url: "https://api.stripe.com".into(),
            auth_env: Some("STRIPE_SECRET_KEY".into()),
        },
        ApiSpec {
            name: "openai".into(),
            description: "OpenAI API — chat completions, embeddings, images".into(),
            spec_url: "https://raw.githubusercontent.com/openai/openai-openapi/master/openapi.yaml".into(),
            base_url: "https://api.openai.com".into(),
            auth_env: Some("OPENAI_API_KEY".into()),
        },
        ApiSpec {
            name: "supabase-management".into(),
            description: "Supabase Management API — projects, databases, auth".into(),
            spec_url: "https://api.apis.guru/v2/specs/supabase.com/analytics/0.0.1/openapi.json".into(),
            base_url: "https://api.supabase.com".into(),
            auth_env: Some("SUPABASE_ACCESS_TOKEN".into()),
        },
        ApiSpec {
            name: "posthog".into(),
            description: "PostHog API — events, persons, feature flags".into(),
            spec_url: "https://raw.githubusercontent.com/PostHog/posthog/master/openapi/bundled_schema.json".into(),
            base_url: "https://eu.posthog.com".into(),
            auth_env: Some("POSTHOG_API_KEY".into()),
        },
        ApiSpec {
            name: "slack".into(),
            description: "Slack Web API — messages, channels, users".into(),
            spec_url: "https://api.apis.guru/v2/specs/slack.com/1.7.0/openapi.json".into(),
            base_url: "https://slack.com/api".into(),
            auth_env: Some("SLACK_TOKEN".into()),
        },
        ApiSpec {
            name: "linear".into(),
            description: "Linear API — issues, projects, teams".into(),
            spec_url: "https://api.apis.guru/v2/specs/linear.app/1.0.0/openapi.json".into(),
            base_url: "https://api.linear.app".into(),
            auth_env: Some("LINEAR_API_KEY".into()),
        },
        ApiSpec {
            name: "cloudflare".into(),
            description: "Cloudflare API — DNS, workers, pages, R2".into(),
            spec_url: "https://raw.githubusercontent.com/cloudflare/api-schemas/main/openapi.json".into(),
            base_url: "https://api.cloudflare.com/client/v4".into(),
            auth_env: Some("CLOUDFLARE_API_TOKEN".into()),
        },
        ApiSpec {
            name: "vercel".into(),
            description: "Vercel API — deployments, projects, domains".into(),
            spec_url: "https://openapi.vercel.sh/".into(),
            base_url: "https://api.vercel.com".into(),
            auth_env: Some("VERCEL_TOKEN".into()),
        },
        ApiSpec {
            name: "sentry".into(),
            description: "Sentry API — issues, events, projects".into(),
            spec_url: "https://api.apis.guru/v2/specs/sentry.io/0.0.1/openapi.json".into(),
            base_url: "https://sentry.io/api/0".into(),
            auth_env: Some("SENTRY_AUTH_TOKEN".into()),
        },
    ]
}

/// Find a popular API by name (case-insensitive).
pub fn find_popular(name: &str) -> Option<ApiSpec> {
    let lower = name.to_lowercase();
    popular_apis().into_iter().find(|a| a.name == lower)
}

/// List all popular API names.
pub fn list_popular() -> Vec<String> {
    popular_apis().into_iter().map(|a| a.name).collect()
}

/// Default cache directory: `~/.sgr-agent/openapi-cache/`
pub fn default_cache_dir() -> PathBuf {
    dirs_like_home().join(".sgr-agent").join("openapi-cache")
}

fn dirs_like_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Get cached spec path for an API name.
pub fn cache_path(cache_dir: &Path, name: &str) -> PathBuf {
    cache_dir.join(format!("{}.json", name))
}

/// Load a cached spec from disk, if it exists.
pub fn load_cached(cache_dir: &Path, name: &str) -> Option<String> {
    let path = cache_path(cache_dir, name);
    std::fs::read_to_string(path).ok()
}

/// Save a spec to cache.
pub fn save_cache(cache_dir: &Path, name: &str, content: &str) -> Result<(), String> {
    std::fs::create_dir_all(cache_dir).map_err(|e| format!("mkdir: {}", e))?;
    let path = cache_path(cache_dir, name);
    std::fs::write(&path, content).map_err(|e| format!("write: {}", e))?;
    Ok(())
}

/// Download a spec from URL. Returns the raw JSON/YAML string.
pub async fn download_spec(url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .user_agent("sgr-agent/0.2")
        .build()
        .map_err(|e| format!("client: {}", e))?;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("fetch: {}", e))?;

    if !resp.status().is_success() {
        return Err(format!("HTTP {}: {}", resp.status(), url));
    }

    let text = resp.text().await.map_err(|e| format!("read: {}", e))?;

    // If YAML, convert to JSON
    if url.ends_with(".yaml") || url.ends_with(".yml") || !text.trim_start().starts_with('{') {
        // Try parsing as JSON first (some .yaml URLs return JSON)
        if serde_json::from_str::<serde_json::Value>(&text).is_ok() {
            return Ok(text);
        }
        return Err(format!(
            "YAML specs not yet supported (need serde_yaml dep). URL: {}",
            url
        ));
    }

    Ok(text)
}

/// Load spec: try cache first, then download and cache.
pub async fn load_or_download(
    cache_dir: &Path,
    name: &str,
    spec_url: &str,
) -> Result<String, String> {
    if let Some(cached) = load_cached(cache_dir, name) {
        return Ok(cached);
    }

    let content = download_spec(spec_url).await?;
    let _ = save_cache(cache_dir, name, &content);
    Ok(content)
}

/// Search APIs.guru directory for an API by name.
/// Returns matching spec URLs.
pub async fn search_apis_guru(query: &str, limit: usize) -> Result<Vec<ApiSpec>, String> {
    let client = reqwest::Client::builder()
        .user_agent("sgr-agent/0.2")
        .build()
        .map_err(|e| format!("client: {}", e))?;

    let resp = client
        .get("https://api.apis.guru/v2/list.json")
        .send()
        .await
        .map_err(|e| format!("fetch: {}", e))?;

    let list: HashMap<String, serde_json::Value> =
        resp.json().await.map_err(|e| format!("parse: {}", e))?;

    let query_lower = query.to_lowercase();
    let mut results = Vec::new();

    for (key, val) in &list {
        let key_lower = key.to_lowercase();
        if !key_lower.contains(&query_lower) {
            continue;
        }

        // Get preferred version
        let preferred = val.get("preferred").and_then(|v| v.as_str()).unwrap_or("");
        let versions = match val.get("versions").and_then(|v| v.as_object()) {
            Some(v) => v,
            None => continue,
        };
        let version = versions.get(preferred).or_else(|| versions.values().next());
        let version = match version {
            Some(v) => v,
            None => continue,
        };

        let spec_url = version
            .get("swaggerUrl")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let title = version
            .get("info")
            .and_then(|i| i.get("title"))
            .and_then(|t| t.as_str())
            .unwrap_or(key);
        let description = version
            .get("info")
            .and_then(|i| i.get("description"))
            .and_then(|d| d.as_str())
            .unwrap_or("");

        // Guess base URL from spec URL or key
        let base_url = format!("https://{}", key.split(':').next().unwrap_or(key));

        results.push(ApiSpec {
            name: key.replace([':', '.'], "_"),
            description: format!("{} — {}", title, truncate_str(description, 80)),
            spec_url: spec_url.to_string(),
            base_url,
            auth_env: None,
        });

        if results.len() >= limit {
            break;
        }
    }

    Ok(results)
}

fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..s
            .char_indices()
            .take(max)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popular_apis_has_entries() {
        let apis = popular_apis();
        assert!(apis.len() >= 8);
    }

    #[test]
    fn find_popular_case_insensitive() {
        assert!(find_popular("GitHub").is_some());
        assert!(find_popular("github").is_some());
        assert!(find_popular("nonexistent").is_none());
    }

    #[test]
    fn cache_path_format() {
        let p = cache_path(Path::new("/tmp/cache"), "github");
        assert_eq!(p, PathBuf::from("/tmp/cache/github.json"));
    }

    #[test]
    fn save_and_load_cache() {
        let dir = tempfile::tempdir().unwrap();
        save_cache(dir.path(), "test-api", r#"{"paths":{}}"#).unwrap();
        let loaded = load_cached(dir.path(), "test-api");
        assert!(loaded.is_some());
        assert_eq!(loaded.unwrap(), r#"{"paths":{}}"#);
    }

    #[test]
    fn load_cached_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_cached(dir.path(), "nonexistent").is_none());
    }

    #[test]
    fn list_popular_names() {
        let names = list_popular();
        assert!(names.contains(&"github".to_string()));
        assert!(names.contains(&"stripe".to_string()));
        assert!(names.contains(&"openai".to_string()));
    }

    #[test]
    fn popular_apis_have_required_fields() {
        for api in popular_apis() {
            assert!(!api.name.is_empty(), "name empty");
            assert!(!api.spec_url.is_empty(), "{} missing spec_url", api.name);
            assert!(!api.base_url.is_empty(), "{} missing base_url", api.name);
            assert!(
                api.spec_url.starts_with("https://"),
                "{} spec_url not https",
                api.name
            );
        }
    }
}
