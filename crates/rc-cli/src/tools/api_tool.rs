//! API tool — call REST APIs via OpenAPI spec.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sgr_agent::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent::context::AgentContext;
use sgr_agent::openapi::{self, ApiRegistry};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct ApiArgs {
    /// Action: "load" (load an API spec), "search" (find endpoints), "call" (execute endpoint), "list" (show loaded APIs)
    pub action: String,
    /// API name (e.g. "github", "stripe", "cloudflare"). Required for load/search/call.
    #[serde(default)]
    pub api_name: Option<String>,
    /// Search query for "search" action (e.g. "create issue")
    #[serde(default)]
    pub query: Option<String>,
    /// Endpoint name for "call" action (e.g. "repos_owner_repo_issues_post")
    #[serde(default)]
    pub endpoint: Option<String>,
    /// Parameters for "call" action as comma-separated key=value pairs (e.g. "owner=foo,repo=bar,state=open")
    #[serde(default)]
    pub params: Option<String>,
    /// Request body JSON string for "call" action (POST/PUT/PATCH), e.g. "{\"title\": \"Bug\"}"
    #[serde(default)]
    pub body: Option<String>,
}

pub struct ApiTool {
    pub registry: Arc<TokioMutex<ApiRegistry>>,
}

#[async_trait::async_trait]
impl Tool for ApiTool {
    fn name(&self) -> &str {
        "api"
    }
    fn description(&self) -> &str {
        "Call any REST API via OpenAPI spec. Actions: 'load' (api_name: github/stripe/cloudflare/...), 'search' (api_name + query), 'call' (api_name + endpoint + params + body), 'list' (show loaded APIs). Load an API first, search for the endpoint, then call it."
    }
    fn parameters_schema(&self) -> serde_json::Value {
        sgr_agent::schema::json_schema_for::<ApiArgs>()
    }

    async fn execute(
        &self,
        args: serde_json::Value,
        _ctx: &mut AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let args: ApiArgs = parse_args(&args)?;
        let mut reg = self.registry.lock().await;
        match args.action.as_str() {
            "list" => {
                let apis = reg.list_apis();
                let mut out = String::new();
                if apis.is_empty() {
                    out.push_str("No APIs loaded yet.\n");
                } else {
                    out.push_str("Loaded APIs:\n");
                    for name in &apis {
                        out.push_str(&format!(
                            "  {} ({} endpoints)\n",
                            name,
                            reg.endpoint_count(name)
                        ));
                    }
                }
                out.push_str("\nAvailable to load:\n");
                for spec in openapi::load_api_registry() {
                    out.push_str(&format!("  {} \u{2014} {}\n", spec.name, spec.description));
                }
                Ok(ToolOutput::text(out))
            }
            "load" => {
                let name = args.api_name.as_deref().unwrap_or("github");
                // Skip if already loaded
                let existing = reg.endpoint_count(name);
                if existing > 0 {
                    return Ok(ToolOutput::text(format!(
                        "{} API ALREADY loaded ({} endpoints). Do NOT call load again.\n\
                         Next step: use api action=search api_name={} query=\"your search\"",
                        name, existing, name
                    )));
                }
                match reg.load_popular(name).await {
                    Ok(count) => Ok(ToolOutput::text(format!(
                        "Loaded {} API: {} endpoints.\n\
                         Next step: use api action=search api_name={} query=\"your search\"",
                        name, count, name
                    ))),
                    Err(e) => {
                        // Show description from registry -- may contain setup hints
                        let hint = openapi::find_popular(name)
                            .map(|a| format!("\nHint: {}", a.description))
                            .unwrap_or_default();
                        Ok(ToolOutput::text(format!(
                            "Failed to load {}: {}{}",
                            name, e, hint
                        )))
                    }
                }
            }
            "search" => {
                let name = args.api_name.as_deref().unwrap_or("github");
                let q = args.query.as_deref().unwrap_or("");
                if reg.endpoint_count(name) == 0 {
                    // Auto-load if not loaded yet
                    if let Err(e) = reg.load_popular(name).await {
                        return Ok(ToolOutput::text(format!(
                            "Failed to auto-load {}: {}",
                            name, e
                        )));
                    }
                }
                let results = reg.search(name, q, 10);
                let out = openapi::format_results(&results);
                Ok(ToolOutput::text(format!(
                    "Search '{}' in {}:\n{}",
                    q, name, out
                )))
            }
            "call" => {
                let name = args.api_name.as_deref().unwrap_or("github");
                let ep = args.endpoint.as_deref().unwrap_or("");
                if ep.is_empty() {
                    return Ok(ToolOutput::text(
                        "Missing 'endpoint' param. Use api search first to find endpoint name.",
                    ));
                }
                // Parse params from "key=val,key2=val2" string
                let param_map: std::collections::HashMap<String, String> = args
                    .params
                    .as_deref()
                    .unwrap_or("")
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .filter_map(|pair| {
                        let mut parts = pair.splitn(2, '=');
                        let key = parts.next()?.trim().to_string();
                        let val = parts.next()?.trim().to_string();
                        Some((key, val))
                    })
                    .collect();

                // Parse body: explicit JSON string, or auto-build from params for POST
                let body_val: Option<serde_json::Value> = args
                    .body
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .and_then(|s| serde_json::from_str(s).ok())
                    .or_else(|| {
                        // For POST/PUT/PATCH: if no body but has params, send params as JSON body
                        let ep_obj = reg.find_endpoint(name, ep)?;
                        let method = ep_obj.method.as_str();
                        if matches!(method, "POST" | "PUT" | "PATCH") && !param_map.is_empty() {
                            // Separate path params from body params
                            let path_params: std::collections::HashSet<&str> = ep_obj
                                .params
                                .iter()
                                .filter(|p| p.location == sgr_agent::openapi::ParamLocation::Path)
                                .map(|p| p.name.as_str())
                                .collect();
                            let body_params: serde_json::Map<String, serde_json::Value> = param_map
                                .iter()
                                .filter(|(k, _)| !path_params.contains(k.as_str()))
                                .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                                .collect();
                            if !body_params.is_empty() {
                                return Some(serde_json::Value::Object(body_params));
                            }
                        }
                        None
                    });

                match reg.call(name, ep, &param_map, body_val.as_ref()).await {
                    Ok(response) => {
                        // Truncate large responses
                        let out = if response.len() > 8000 {
                            format!(
                                "{}...\n\n(truncated, {} bytes total)",
                                &response[..8000],
                                response.len()
                            )
                        } else {
                            response
                        };
                        Ok(ToolOutput::text(out))
                    }
                    Err(e) => Ok(ToolOutput::text(format!("API call failed: {}", e))),
                }
            }
            other => Ok(ToolOutput::text(format!(
                "Unknown api action: '{}'. Use: load, search, call, list",
                other
            ))),
        }
    }
}
