//! OpenAPI → Agent Tool: convert any API spec into a searchable, callable tool.
//!
//! Instead of 845 individual MCP tools for GitHub API, one `api` tool:
//! - `api search "create issue"` → fuzzy-find endpoints
//! - `api call repos_owner_repo_issues_post --owner=foo --repo=bar --title="bug"` → execute
//!
//! ## Usage
//!
//! ```ignore
//! use sgr_agent::openapi::{ApiRegistry, ApiAuth};
//!
//! let mut registry = ApiRegistry::new();
//! registry.add_api("github", "https://api.github.com", &spec_json, ApiAuth::Bearer("ghp_xxx".into())).unwrap();
//! let results = registry.search("github", "create issue", 5);
//! ```

pub mod caller;
pub mod registry;
pub mod search;
pub mod spec;

pub use caller::ApiAuth;
pub use registry::{
    ApiSpec, default_cache_dir, download_spec, find_popular, list_popular, load_api_registry,
    load_or_download, popular_apis, search_apis_guru,
};
pub use search::{SearchResult, format_results, search_endpoints};
pub use spec::{Endpoint, Param, ParamLocation, filter_endpoints, parse_spec};

use std::collections::HashMap;

/// Registry of loaded API specs. Each API has a name, base URL, and parsed endpoints.
#[derive(Default)]
pub struct ApiRegistry {
    apis: HashMap<String, LoadedApi>,
}

struct LoadedApi {
    base_url: String,
    endpoints: Vec<Endpoint>,
    auth: ApiAuth,
}

impl ApiRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add an API from a JSON spec string.
    pub fn add_api(
        &mut self,
        name: &str,
        base_url: &str,
        spec_json: &str,
        auth: ApiAuth,
    ) -> Result<usize, String> {
        let spec: serde_json::Value =
            serde_json::from_str(spec_json).map_err(|e| format!("Invalid JSON: {}", e))?;
        let endpoints = parse_spec(&spec);
        let count = endpoints.len();
        self.apis.insert(
            name.to_string(),
            LoadedApi {
                base_url: base_url.to_string(),
                endpoints,
                auth,
            },
        );
        Ok(count)
    }

    /// Add an API from a pre-parsed spec Value.
    pub fn add_api_from_value(
        &mut self,
        name: &str,
        base_url: &str,
        spec: &serde_json::Value,
        auth: ApiAuth,
    ) -> usize {
        let endpoints = parse_spec(spec);
        let count = endpoints.len();
        self.apis.insert(
            name.to_string(),
            LoadedApi {
                base_url: base_url.to_string(),
                endpoints,
                auth,
            },
        );
        count
    }

    /// Load a popular API by name — auto-download + cache.
    /// Returns endpoint count or error.
    pub async fn load_popular(&mut self, name: &str) -> Result<usize, String> {
        let api_spec = find_popular(name)
            .ok_or_else(|| format!("Unknown API: {}. Available: {:?}", name, list_popular()))?;
        self.load_spec(&api_spec).await
    }

    /// Load any ApiSpec — download (or use cache) and register.
    pub async fn load_spec(&mut self, api_spec: &ApiSpec) -> Result<usize, String> {
        let cache_dir = default_cache_dir();
        let json = load_or_download(&cache_dir, &api_spec.name, &api_spec.spec_url).await?;

        // Auto-detect auth from env var
        let auth = if let Some(ref env_var) = api_spec.auth_env {
            match std::env::var(env_var) {
                Ok(token) if !token.is_empty() => ApiAuth::Bearer(token),
                _ => ApiAuth::None,
            }
        } else {
            ApiAuth::None
        };

        self.add_api(&api_spec.name, &api_spec.base_url, &json, auth)
    }

    /// List all loaded API names.
    pub fn list_apis(&self) -> Vec<&str> {
        self.apis.keys().map(|s| s.as_str()).collect()
    }

    /// Get endpoint count for an API.
    pub fn endpoint_count(&self, api_name: &str) -> usize {
        self.apis
            .get(api_name)
            .map(|a| a.endpoints.len())
            .unwrap_or(0)
    }

    /// Search endpoints within a specific API.
    pub fn search(&self, api_name: &str, query: &str, limit: usize) -> Vec<SearchResult> {
        match self.apis.get(api_name) {
            Some(api) => search_endpoints(&api.endpoints, query, limit),
            None => Vec::new(),
        }
    }

    /// Search across ALL loaded APIs.
    pub fn search_all(&self, query: &str, limit: usize) -> Vec<(String, SearchResult)> {
        let mut all: Vec<(String, SearchResult)> = Vec::new();
        for (name, api) in &self.apis {
            for r in search_endpoints(&api.endpoints, query, limit) {
                all.push((name.clone(), r));
            }
        }
        all.sort_by(|a, b| b.1.score.cmp(&a.1.score));
        all.truncate(limit);
        all
    }

    /// Find an endpoint by name within an API.
    pub fn find_endpoint(&self, api_name: &str, endpoint_name: &str) -> Option<&Endpoint> {
        self.apis
            .get(api_name)?
            .endpoints
            .iter()
            .find(|e| e.name == endpoint_name)
    }

    /// Call an endpoint by name.
    pub async fn call(
        &self,
        api_name: &str,
        endpoint_name: &str,
        params: &HashMap<String, String>,
        body: Option<&serde_json::Value>,
    ) -> Result<String, String> {
        let api = self
            .apis
            .get(api_name)
            .ok_or_else(|| format!("API not found: {}", api_name))?;
        let endpoint = api
            .endpoints
            .iter()
            .find(|e| e.name == endpoint_name)
            .ok_or_else(|| format!("Endpoint not found: {}", endpoint_name))?;

        caller::call_api(&api.base_url, endpoint, params, body, &api.auth).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn github_spec() -> String {
        json!({
            "paths": {
                "/repos/{owner}/{repo}": {
                    "get": {
                        "summary": "Get a repository",
                        "parameters": [
                            { "name": "owner", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "repo", "in": "path", "required": true, "schema": { "type": "string" } }
                        ]
                    }
                },
                "/repos/{owner}/{repo}/issues": {
                    "get": {
                        "summary": "List issues",
                        "parameters": [
                            { "name": "owner", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "repo", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "state", "in": "query", "schema": { "type": "string" } }
                        ]
                    },
                    "post": {
                        "summary": "Create an issue",
                        "parameters": [
                            { "name": "owner", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "repo", "in": "path", "required": true, "schema": { "type": "string" } }
                        ]
                    }
                },
                "/users": {
                    "get": { "summary": "List users", "parameters": [] }
                }
            }
        })
        .to_string()
    }

    #[test]
    fn add_api_and_count() {
        let mut reg = ApiRegistry::new();
        let count = reg
            .add_api(
                "github",
                "https://api.github.com",
                &github_spec(),
                ApiAuth::None,
            )
            .unwrap();
        assert_eq!(count, 4);
        assert_eq!(reg.endpoint_count("github"), 4);
    }

    #[test]
    fn list_apis() {
        let mut reg = ApiRegistry::new();
        reg.add_api(
            "github",
            "https://api.github.com",
            &github_spec(),
            ApiAuth::None,
        )
        .unwrap();
        let names = reg.list_apis();
        assert_eq!(names, vec!["github"]);
    }

    #[test]
    fn find_endpoint_by_name() {
        let mut reg = ApiRegistry::new();
        reg.add_api(
            "gh",
            "https://api.github.com",
            &github_spec(),
            ApiAuth::None,
        )
        .unwrap();
        let ep = reg.find_endpoint("gh", "repos_owner_repo_issues_post");
        assert!(ep.is_some());
        assert_eq!(ep.unwrap().method, "POST");
    }

    #[test]
    fn find_nonexistent_endpoint() {
        let mut reg = ApiRegistry::new();
        reg.add_api(
            "gh",
            "https://api.github.com",
            &github_spec(),
            ApiAuth::None,
        )
        .unwrap();
        assert!(reg.find_endpoint("gh", "nonexistent").is_none());
        assert!(reg.find_endpoint("nope", "anything").is_none());
    }

    #[test]
    fn search_within_api() {
        let mut reg = ApiRegistry::new();
        reg.add_api(
            "gh",
            "https://api.github.com",
            &github_spec(),
            ApiAuth::None,
        )
        .unwrap();
        let results = reg.search("gh", "issue", 5);
        assert!(!results.is_empty());
    }

    #[test]
    fn search_nonexistent_api() {
        let reg = ApiRegistry::new();
        let results = reg.search("nope", "test", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn invalid_json_returns_error() {
        let mut reg = ApiRegistry::new();
        let err = reg
            .add_api("bad", "https://example.com", "not json", ApiAuth::None)
            .unwrap_err();
        assert!(err.contains("Invalid JSON"));
    }

    #[test]
    fn search_all_across_apis() {
        let mut reg = ApiRegistry::new();
        reg.add_api(
            "gh",
            "https://api.github.com",
            &github_spec(),
            ApiAuth::None,
        )
        .unwrap();
        reg.add_api(
            "other",
            "https://other.com",
            &json!({"paths": {"/items": {"get": {"summary": "List items"}}}}).to_string(),
            ApiAuth::None,
        )
        .unwrap();
        let results = reg.search_all("list", 10);
        assert!(results.len() >= 2); // "List issues" + "List users" + "List items"
    }
}
