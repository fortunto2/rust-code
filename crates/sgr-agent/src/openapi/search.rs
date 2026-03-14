//! Fuzzy search over API endpoints using nucleo.
//!
//! Agent calls `api search "create issue"` → gets top-K matching endpoints.

use super::spec::Endpoint;

/// Search result with score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub name: String,
    pub method: String,
    pub path: String,
    pub description: String,
    pub score: u32,
}

/// Search endpoints by fuzzy query. Returns top `limit` results sorted by score.
///
/// Builds a searchable string for each endpoint:
/// `name method path description param_names`
/// Then fuzzy-matches the query against it using nucleo.
#[cfg(feature = "search")]
pub fn search_endpoints(endpoints: &[Endpoint], query: &str, limit: usize) -> Vec<SearchResult> {
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config, Matcher, Utf32Str};

    if query.is_empty() || endpoints.is_empty() {
        return Vec::new();
    }

    let mut matcher = Matcher::new(Config::DEFAULT.match_paths());
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(u32, usize)> = Vec::new();
    let mut buf = Vec::new();

    for (i, ep) in endpoints.iter().enumerate() {
        let searchable = build_searchable_str(ep);
        let haystack = Utf32Str::new(&searchable, &mut buf);
        if let Some(score) = pattern.score(haystack, &mut matcher) {
            scored.push((score, i));
        }
    }

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(limit);

    scored
        .into_iter()
        .map(|(score, idx)| {
            let ep = &endpoints[idx];
            SearchResult {
                name: ep.name.clone(),
                method: ep.method.clone(),
                path: ep.path.clone(),
                description: ep.description.clone(),
                score,
            }
        })
        .collect()
}

/// Simple substring search fallback (no nucleo dependency).
#[cfg(not(feature = "search"))]
pub fn search_endpoints(endpoints: &[Endpoint], query: &str, limit: usize) -> Vec<SearchResult> {
    let query_lower = query.to_lowercase();
    let mut results: Vec<SearchResult> = Vec::new();

    for ep in endpoints {
        let searchable = build_searchable_str(ep).to_lowercase();
        if searchable.contains(&query_lower) {
            results.push(SearchResult {
                name: ep.name.clone(),
                method: ep.method.clone(),
                path: ep.path.clone(),
                description: ep.description.clone(),
                score: 100,
            });
            if results.len() >= limit {
                break;
            }
        }
    }

    results
}

/// Build a searchable string from endpoint fields.
fn build_searchable_str(ep: &Endpoint) -> String {
    let mut parts = vec![
        ep.name.replace('_', " "),
        ep.method.clone(),
        ep.path.replace('/', " ").replace(['{', '}'], ""),
    ];
    if !ep.description.is_empty() {
        parts.push(ep.description.clone());
    }
    for p in &ep.params {
        parts.push(p.name.clone());
        if !p.description.is_empty() {
            parts.push(p.description.clone());
        }
    }
    parts.join(" ")
}

/// Format search results for display to the agent.
pub fn format_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "No endpoints found.".to_string();
    }

    let mut out = String::new();
    for r in results {
        out.push_str(&format!(
            "  {} {} {} — {}\n",
            r.method, r.name, r.path, r.description
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::spec::{parse_spec, Endpoint, Param, ParamLocation};
    use serde_json::json;

    fn test_endpoints() -> Vec<Endpoint> {
        let spec = json!({
            "paths": {
                "/users": {
                    "get": { "summary": "List all users", "parameters": [] },
                    "post": { "summary": "Create a new user", "parameters": [] }
                },
                "/repos/{owner}/{repo}/issues": {
                    "get": {
                        "summary": "List repository issues",
                        "parameters": [
                            { "name": "owner", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "repo", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "state", "in": "query", "schema": { "type": "string" }, "description": "Filter by state" }
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
                "/repos/{owner}/{repo}/pulls": {
                    "get": { "summary": "List pull requests", "parameters": [] }
                }
            }
        });
        parse_spec(&spec)
    }

    #[test]
    fn search_finds_relevant() {
        let eps = test_endpoints();
        let results = search_endpoints(&eps, "create issue", 5);
        assert!(!results.is_empty());
        // "Create an issue" should rank high
        assert!(results[0].description.contains("issue") || results[0].name.contains("issue"));
    }

    #[test]
    fn search_empty_query() {
        let eps = test_endpoints();
        let results = search_endpoints(&eps, "", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn search_respects_limit() {
        let eps = test_endpoints();
        let results = search_endpoints(&eps, "repo", 2);
        assert!(results.len() <= 2);
    }

    #[test]
    fn format_results_empty() {
        assert_eq!(format_results(&[]), "No endpoints found.");
    }

    #[test]
    fn format_results_shows_method_and_path() {
        let results = vec![SearchResult {
            name: "users_get".into(),
            method: "GET".into(),
            path: "/users".into(),
            description: "List users".into(),
            score: 100,
        }];
        let out = format_results(&results);
        assert!(out.contains("GET"));
        assert!(out.contains("/users"));
        assert!(out.contains("List users"));
    }

    #[test]
    fn searchable_string_includes_all_fields() {
        let ep = Endpoint {
            name: "users_get".into(),
            method: "GET".into(),
            path: "/users".into(),
            description: "List all users".into(),
            params: vec![Param {
                name: "page".into(),
                location: ParamLocation::Query,
                required: false,
                param_type: "integer".into(),
                description: "Page number".into(),
            }],
        };
        let s = build_searchable_str(&ep);
        assert!(s.contains("users"));
        assert!(s.contains("GET"));
        assert!(s.contains("List all users"));
        assert!(s.contains("page"));
        assert!(s.contains("Page number"));
    }
}
