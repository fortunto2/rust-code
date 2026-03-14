//! HTTP caller — execute API requests from endpoint definitions.

use super::spec::{Endpoint, ParamLocation};
use std::collections::HashMap;

/// Auth configuration for API calls.
#[derive(Debug, Clone)]
pub enum ApiAuth {
    None,
    Bearer(String),
    Basic(String),          // "user:pass"
    Header(String, String), // custom header name + value
}

/// Build the full URL from an endpoint, base URL, and parameter values.
///
/// Path params are substituted in the URL template.
/// Query params are appended as `?key=value&...`.
pub fn build_url(
    base_url: &str,
    endpoint: &Endpoint,
    params: &HashMap<String, String>,
) -> Result<String, String> {
    // Check required params
    for p in &endpoint.params {
        if p.required && !params.contains_key(&p.name) {
            return Err(format!("Missing required parameter: {}", p.name));
        }
    }

    // Substitute path params
    let mut path = endpoint.path.clone();
    for p in &endpoint.params {
        if p.location == ParamLocation::Path {
            if let Some(value) = params.get(&p.name) {
                let token = format!("{{{}}}", p.name);
                path = path.replace(&token, value);
            }
        }
    }

    let base = base_url.trim_end_matches('/');
    let mut url = format!("{}{}", base, path);

    // Append query params
    let query_parts: Vec<String> = endpoint
        .params
        .iter()
        .filter(|p| p.location == ParamLocation::Query)
        .filter_map(|p| {
            params
                .get(&p.name)
                .map(|v| format!("{}={}", p.name, urlencod(v)))
        })
        .collect();

    if !query_parts.is_empty() {
        url.push('?');
        url.push_str(&query_parts.join("&"));
    }

    Ok(url)
}

/// Simple percent-encoding for query values.
fn urlencod(s: &str) -> String {
    s.replace('%', "%25")
        .replace(' ', "%20")
        .replace('&', "%26")
        .replace('=', "%3D")
        .replace('+', "%2B")
        .replace('#', "%23")
}

/// Execute an API call. Returns the response body as string.
pub async fn call_api(
    base_url: &str,
    endpoint: &Endpoint,
    params: &HashMap<String, String>,
    body: Option<&serde_json::Value>,
    auth: &ApiAuth,
) -> Result<String, String> {
    let url = build_url(base_url, endpoint, params)?;

    let client = reqwest::Client::new();
    let mut req = match endpoint.method.as_str() {
        "GET" => client.get(&url),
        "POST" => client.post(&url),
        "PUT" => client.put(&url),
        "DELETE" => client.delete(&url),
        "PATCH" => client.patch(&url),
        "HEAD" => client.head(&url),
        other => return Err(format!("Unsupported method: {}", other)),
    };

    // Auth
    match auth {
        ApiAuth::None => {}
        ApiAuth::Bearer(token) => {
            req = req.header("Authorization", format!("Bearer {}", token));
        }
        ApiAuth::Basic(credentials) => {
            let encoded = simple_base64(credentials.as_bytes());
            req = req.header("Authorization", format!("Basic {}", encoded));
        }
        ApiAuth::Header(name, value) => {
            req = req.header(name, value);
        }
    }

    // Body
    if let Some(body_val) = body {
        req = req
            .header("Content-Type", "application/json")
            .json(body_val);
    }

    let response = req.send().await.map_err(|e| format!("HTTP error: {}", e))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("Read error: {}", e))?;

    if status.is_success() {
        Ok(text)
    } else {
        Err(format!("HTTP {} — {}", status, truncate(&text, 500)))
    }
}

/// Minimal base64 encoder (no external dep needed for just auth headers).
fn simple_base64(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((n >> 18) & 63) as usize] as char);
        result.push(CHARS[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((n >> 6) & 63) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(n & 63) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::openapi::spec::{Endpoint, Param, ParamLocation};

    fn issue_endpoint() -> Endpoint {
        Endpoint {
            name: "repos_owner_repo_issues_get".into(),
            method: "GET".into(),
            path: "/repos/{owner}/{repo}/issues".into(),
            description: "List issues".into(),
            params: vec![
                Param {
                    name: "owner".into(),
                    location: ParamLocation::Path,
                    required: true,
                    param_type: "string".into(),
                    description: "".into(),
                },
                Param {
                    name: "repo".into(),
                    location: ParamLocation::Path,
                    required: true,
                    param_type: "string".into(),
                    description: "".into(),
                },
                Param {
                    name: "state".into(),
                    location: ParamLocation::Query,
                    required: false,
                    param_type: "string".into(),
                    description: "open/closed/all".into(),
                },
            ],
        }
    }

    #[test]
    fn build_url_substitutes_path_params() {
        let ep = issue_endpoint();
        let mut params = HashMap::new();
        params.insert("owner".into(), "rust-lang".into());
        params.insert("repo".into(), "rust".into());

        let url = build_url("https://api.github.com", &ep, &params).unwrap();
        assert_eq!(url, "https://api.github.com/repos/rust-lang/rust/issues");
    }

    #[test]
    fn build_url_with_query_params() {
        let ep = issue_endpoint();
        let mut params = HashMap::new();
        params.insert("owner".into(), "foo".into());
        params.insert("repo".into(), "bar".into());
        params.insert("state".into(), "open".into());

        let url = build_url("https://api.github.com", &ep, &params).unwrap();
        assert!(url.contains("?state=open"));
    }

    #[test]
    fn build_url_missing_required_param() {
        let ep = issue_endpoint();
        let params = HashMap::new();
        let err = build_url("https://api.github.com", &ep, &params).unwrap_err();
        assert!(err.contains("Missing required parameter: owner"));
    }

    #[test]
    fn build_url_trailing_slash_base() {
        let ep = Endpoint {
            name: "test".into(),
            method: "GET".into(),
            path: "/test".into(),
            description: "".into(),
            params: vec![],
        };
        let url = build_url("https://example.com/", &ep, &HashMap::new()).unwrap();
        assert_eq!(url, "https://example.com/test");
    }

    #[test]
    fn urlencod_special_chars() {
        assert_eq!(urlencod("hello world"), "hello%20world");
        assert_eq!(urlencod("a&b=c"), "a%26b%3Dc");
    }
}
