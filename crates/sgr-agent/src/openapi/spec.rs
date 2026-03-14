//! OpenAPI spec parser — extract endpoints from JSON/YAML specs.
//!
//! Parses OpenAPI 3.x specs into a flat list of [`Endpoint`]s.
//! Each endpoint = one HTTP method + path + parameters + description.

use serde_json::Value;

/// A single API endpoint extracted from an OpenAPI spec.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// CLI-friendly name: `repos_owner_repo_issues_post`
    pub name: String,
    /// HTTP method (uppercase): GET, POST, PUT, DELETE, PATCH
    pub method: String,
    /// Path template: `/repos/{owner}/{repo}/issues`
    pub path: String,
    /// Human-readable description (from summary or description)
    pub description: String,
    /// Parameters (path + query)
    pub params: Vec<Param>,
}

/// A single parameter for an endpoint.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    /// "path" or "query"
    pub location: ParamLocation,
    pub required: bool,
    pub param_type: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParamLocation {
    Path,
    Query,
}

/// Parse an OpenAPI spec (as JSON Value) into a list of endpoints.
///
/// Supports OpenAPI 3.x format. Extracts paths, methods, parameters.
/// Resolves `$ref` references for parameters and path items.
pub fn parse_spec(spec: &Value) -> Vec<Endpoint> {
    let paths = match spec.get("paths").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return Vec::new(),
    };

    let methods = ["get", "post", "put", "delete", "patch", "head", "options"];

    // Collect all path→methods mapping first (to decide if we need method suffix)
    let mut path_method_count: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for (path, item) in paths {
        let item_obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };
        let count = methods
            .iter()
            .filter(|m| item_obj.contains_key(**m))
            .count();
        path_method_count.insert(path.as_str(), count);
    }

    let mut endpoints = Vec::new();

    for (path, item) in paths {
        let item_obj = match item.as_object() {
            Some(o) => o,
            None => continue,
        };

        let multiple_methods = path_method_count.get(path.as_str()).copied().unwrap_or(0) > 1;

        // Path-level parameters (shared across all methods on this path)
        let path_params = item_obj
            .get("parameters")
            .map(|p| extract_params_with_refs(p, spec))
            .unwrap_or_default();

        for method in &methods {
            let operation = match item_obj.get(*method) {
                Some(op) => op,
                None => continue,
            };

            let base_name = path_to_command_name(path);
            let name = if multiple_methods {
                format!("{}_{}", base_name, method)
            } else {
                base_name
            };

            let description = operation
                .get("summary")
                .or_else(|| operation.get("description"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            // Merge path-level + operation-level params (operation overrides path)
            let op_params =
                extract_params_with_refs(operation.get("parameters").unwrap_or(&Value::Null), spec);
            let params = merge_params(&path_params, &op_params);

            endpoints.push(Endpoint {
                name,
                method: method.to_uppercase(),
                path: path.clone(),
                description,
                params,
            });
        }
    }

    endpoints
}

/// Convert path `/repos/{owner}/{repo}/issues` → `repos_owner_repo_issues`
fn path_to_command_name(path: &str) -> String {
    path.split('/')
        .filter(|s| !s.is_empty())
        .map(|s| {
            if s.starts_with('{') && s.ends_with('}') {
                &s[1..s.len() - 1]
            } else {
                s
            }
        })
        .collect::<Vec<_>>()
        .join("_")
}

/// Extract parameters from a Value, resolving `$ref` references.
fn extract_params_with_refs(params_val: &Value, root: &Value) -> Vec<Param> {
    let params_arr = match params_val.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    params_arr
        .iter()
        .filter_map(|p| {
            // Resolve $ref if present
            let resolved = if let Some(ref_str) = p.get("$ref").and_then(|r| r.as_str()) {
                resolve_ref(root, ref_str)?
            } else {
                p
            };

            let name = resolved.get("name")?.as_str()?.to_string();
            let location_str = resolved.get("in")?.as_str()?;
            let location = match location_str {
                "path" => ParamLocation::Path,
                "query" => ParamLocation::Query,
                _ => return None, // skip header, cookie params
            };
            let required = resolved
                .get("required")
                .and_then(|r| r.as_bool())
                .unwrap_or(location == ParamLocation::Path); // path params are implicitly required
            let param_type = resolved
                .get("schema")
                .and_then(|s| {
                    // Also resolve schema $ref
                    if let Some(sr) = s.get("$ref").and_then(|r| r.as_str()) {
                        resolve_ref(root, sr)
                            .and_then(|rs| rs.get("type"))
                            .and_then(|t| t.as_str())
                    } else {
                        s.get("type").and_then(|t| t.as_str())
                    }
                })
                .unwrap_or("string")
                .to_string();
            let description = resolved
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();

            Some(Param {
                name,
                location,
                required,
                param_type,
                description,
            })
        })
        .collect()
}

/// Resolve a JSON `$ref` pointer like `#/components/parameters/owner`.
fn resolve_ref<'a>(root: &'a Value, ref_str: &str) -> Option<&'a Value> {
    let path = ref_str.strip_prefix("#/")?;
    let mut current = root;
    for segment in path.split('/') {
        // Handle JSON Pointer escaping: ~1 → /, ~0 → ~
        let unescaped = segment.replace("~1", "/").replace("~0", "~");
        current = current.get(&unescaped)?;
    }
    Some(current)
}

/// Merge path-level and operation-level params.
/// Operation params override path params with the same name+location.
fn merge_params(path_params: &[Param], op_params: &[Param]) -> Vec<Param> {
    let mut result: Vec<Param> = Vec::new();

    // Start with path-level params
    for pp in path_params {
        // Check if operation overrides this param
        let overridden = op_params
            .iter()
            .any(|op| op.name == pp.name && op.location == pp.location);
        if !overridden {
            result.push(pp.clone());
        }
    }

    // Add all operation-level params
    result.extend(op_params.iter().cloned());
    result
}

/// Filter endpoints by include/exclude patterns.
/// Patterns are `method:path` (e.g. `get:/repos/{owner}/{repo}`).
pub fn filter_endpoints(
    endpoints: Vec<Endpoint>,
    include: &[String],
    exclude: &[String],
) -> Vec<Endpoint> {
    let exclude_set: std::collections::HashSet<&str> = exclude.iter().map(|s| s.as_str()).collect();

    endpoints
        .into_iter()
        .filter(|ep| {
            let key = format!("{}:{}", ep.method.to_lowercase(), ep.path);
            if exclude_set.contains(key.as_str()) {
                return false;
            }
            if include.is_empty() {
                return true;
            }
            include.iter().any(|i| i == &key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample_spec() -> Value {
        json!({
            "openapi": "3.0.0",
            "info": { "title": "Test API", "version": "1.0" },
            "paths": {
                "/users": {
                    "get": {
                        "summary": "List users",
                        "parameters": [
                            {
                                "name": "page",
                                "in": "query",
                                "required": false,
                                "schema": { "type": "integer" },
                                "description": "Page number"
                            },
                            {
                                "name": "limit",
                                "in": "query",
                                "schema": { "type": "integer" }
                            }
                        ]
                    },
                    "post": {
                        "summary": "Create user",
                        "parameters": []
                    }
                },
                "/users/{id}": {
                    "get": {
                        "summary": "Get user by ID",
                        "parameters": [
                            {
                                "name": "id",
                                "in": "path",
                                "required": true,
                                "schema": { "type": "integer" },
                                "description": "User ID"
                            }
                        ]
                    }
                },
                "/repos/{owner}/{repo}/issues": {
                    "get": {
                        "summary": "List issues",
                        "parameters": [
                            { "name": "owner", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "repo", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "state", "in": "query", "schema": { "type": "string" }, "description": "open/closed/all" }
                        ]
                    },
                    "post": {
                        "description": "Create an issue",
                        "parameters": [
                            { "name": "owner", "in": "path", "required": true, "schema": { "type": "string" } },
                            { "name": "repo", "in": "path", "required": true, "schema": { "type": "string" } }
                        ]
                    }
                }
            }
        })
    }

    #[test]
    fn parse_extracts_all_endpoints() {
        let endpoints = parse_spec(&sample_spec());
        assert_eq!(endpoints.len(), 5);
    }

    #[test]
    fn single_method_path_no_suffix() {
        let endpoints = parse_spec(&sample_spec());
        let user_by_id = endpoints.iter().find(|e| e.path == "/users/{id}").unwrap();
        assert_eq!(user_by_id.name, "users_id");
        assert_eq!(user_by_id.method, "GET");
    }

    #[test]
    fn multiple_methods_get_suffix() {
        let endpoints = parse_spec(&sample_spec());
        let users: Vec<_> = endpoints.iter().filter(|e| e.path == "/users").collect();
        assert_eq!(users.len(), 2);
        let names: Vec<&str> = users.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"users_get"));
        assert!(names.contains(&"users_post"));
    }

    #[test]
    fn nested_path_command_name() {
        let endpoints = parse_spec(&sample_spec());
        let issues: Vec<_> = endpoints
            .iter()
            .filter(|e| e.path == "/repos/{owner}/{repo}/issues")
            .collect();
        assert!(issues
            .iter()
            .any(|e| e.name == "repos_owner_repo_issues_get"));
        assert!(issues
            .iter()
            .any(|e| e.name == "repos_owner_repo_issues_post"));
    }

    #[test]
    fn params_extracted() {
        let endpoints = parse_spec(&sample_spec());
        let user_by_id = endpoints.iter().find(|e| e.path == "/users/{id}").unwrap();
        assert_eq!(user_by_id.params.len(), 1);
        assert_eq!(user_by_id.params[0].name, "id");
        assert_eq!(user_by_id.params[0].location, ParamLocation::Path);
        assert!(user_by_id.params[0].required);
    }

    #[test]
    fn description_from_summary_or_description() {
        let endpoints = parse_spec(&sample_spec());
        let list_users = endpoints.iter().find(|e| e.name == "users_get").unwrap();
        assert_eq!(list_users.description, "List users");

        let create_issue = endpoints
            .iter()
            .find(|e| e.name == "repos_owner_repo_issues_post")
            .unwrap();
        assert_eq!(create_issue.description, "Create an issue");
    }

    #[test]
    fn path_to_name_strips_braces() {
        assert_eq!(path_to_command_name("/a/{b}/c"), "a_b_c");
        assert_eq!(path_to_command_name("/"), "");
        assert_eq!(path_to_command_name("/simple"), "simple");
    }

    #[test]
    fn filter_exclude() {
        let endpoints = parse_spec(&sample_spec());
        let filtered = filter_endpoints(endpoints, &[], &["post:/users".to_string()]);
        assert!(!filtered.iter().any(|e| e.name == "users_post"));
        assert!(filtered.iter().any(|e| e.name == "users_get"));
    }

    #[test]
    fn filter_include() {
        let endpoints = parse_spec(&sample_spec());
        let filtered = filter_endpoints(endpoints, &["get:/users".to_string()], &[]);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].name, "users_get");
    }

    #[test]
    fn empty_spec_returns_empty() {
        let endpoints = parse_spec(&json!({}));
        assert!(endpoints.is_empty());
    }

    #[test]
    fn header_params_skipped() {
        let spec = json!({
            "paths": {
                "/test": {
                    "get": {
                        "parameters": [
                            { "name": "X-Token", "in": "header", "schema": { "type": "string" } },
                            { "name": "q", "in": "query", "schema": { "type": "string" } }
                        ]
                    }
                }
            }
        });
        let endpoints = parse_spec(&spec);
        assert_eq!(endpoints[0].params.len(), 1);
        assert_eq!(endpoints[0].params[0].name, "q");
    }

    #[test]
    fn ref_params_resolved() {
        let spec = json!({
            "components": {
                "parameters": {
                    "owner": {
                        "name": "owner",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "The account owner"
                    },
                    "repo": {
                        "name": "repo",
                        "in": "path",
                        "required": true,
                        "schema": { "type": "string" },
                        "description": "The repository name"
                    },
                    "per_page": {
                        "name": "per_page",
                        "in": "query",
                        "schema": { "type": "integer" },
                        "description": "Results per page (max 100)"
                    }
                }
            },
            "paths": {
                "/repos/{owner}/{repo}": {
                    "get": {
                        "summary": "Get a repository",
                        "parameters": [
                            { "$ref": "#/components/parameters/owner" },
                            { "$ref": "#/components/parameters/repo" },
                            { "$ref": "#/components/parameters/per_page" }
                        ]
                    }
                }
            }
        });
        let endpoints = parse_spec(&spec);
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].params.len(), 3);
        assert_eq!(endpoints[0].params[0].name, "owner");
        assert_eq!(endpoints[0].params[0].description, "The account owner");
        assert!(endpoints[0].params[0].required);
        assert_eq!(endpoints[0].params[1].name, "repo");
        assert_eq!(endpoints[0].params[2].name, "per_page");
        assert_eq!(endpoints[0].params[2].param_type, "integer");
    }

    #[test]
    fn path_level_params_merged() {
        let spec = json!({
            "paths": {
                "/repos/{owner}/{repo}/issues": {
                    "parameters": [
                        { "name": "owner", "in": "path", "required": true, "schema": { "type": "string" } },
                        { "name": "repo", "in": "path", "required": true, "schema": { "type": "string" } }
                    ],
                    "get": {
                        "summary": "List issues",
                        "parameters": [
                            { "name": "state", "in": "query", "schema": { "type": "string" } }
                        ]
                    },
                    "post": {
                        "summary": "Create issue"
                    }
                }
            }
        });
        let endpoints = parse_spec(&spec);
        let get = endpoints.iter().find(|e| e.method == "GET").unwrap();
        // GET should have owner, repo (from path-level) + state (from operation)
        assert_eq!(get.params.len(), 3);

        let post = endpoints.iter().find(|e| e.method == "POST").unwrap();
        // POST should inherit owner, repo from path-level
        assert_eq!(post.params.len(), 2);
        assert_eq!(post.params[0].name, "owner");
    }

    #[test]
    fn operation_params_override_path_params() {
        let spec = json!({
            "paths": {
                "/items/{id}": {
                    "parameters": [
                        { "name": "id", "in": "path", "required": true, "schema": { "type": "integer" }, "description": "generic" }
                    ],
                    "get": {
                        "parameters": [
                            { "name": "id", "in": "path", "required": true, "schema": { "type": "string" }, "description": "overridden" }
                        ]
                    }
                }
            }
        });
        let endpoints = parse_spec(&spec);
        assert_eq!(endpoints[0].params.len(), 1);
        assert_eq!(endpoints[0].params[0].description, "overridden");
        assert_eq!(endpoints[0].params[0].param_type, "string");
    }

    #[test]
    fn resolve_ref_basic() {
        let root = json!({
            "components": {
                "parameters": {
                    "foo": { "name": "foo", "in": "query" }
                }
            }
        });
        let resolved = resolve_ref(&root, "#/components/parameters/foo");
        assert!(resolved.is_some());
        assert_eq!(resolved.unwrap().get("name").unwrap().as_str(), Some("foo"));
    }

    #[test]
    fn resolve_ref_missing() {
        let root = json!({});
        assert!(resolve_ref(&root, "#/components/parameters/missing").is_none());
    }

    #[test]
    fn path_params_implicitly_required() {
        let spec = json!({
            "paths": {
                "/items/{id}": {
                    "get": {
                        "parameters": [
                            { "name": "id", "in": "path", "schema": { "type": "integer" } }
                        ]
                    }
                }
            }
        });
        let endpoints = parse_spec(&spec);
        // Path params without explicit "required" should be true
        assert!(endpoints[0].params[0].required);
    }
}
