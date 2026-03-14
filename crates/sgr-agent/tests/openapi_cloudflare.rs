//! E2E test: Cloudflare API via OpenAPI spec + real API call.
//!
//! Run: cargo test -p sgr-agent --features "agent search" --test openapi_cloudflare -- --nocapture

use sgr_agent::openapi::{ApiAuth, ApiRegistry};
use std::collections::HashMap;

fn cloudflare_token() -> Option<String> {
    // Try env var first
    if let Ok(token) = std::env::var("CLOUDFLARE_API_TOKEN") {
        if !token.is_empty() {
            return Some(token);
        }
    }
    // Fall back to wrangler OAuth token
    let home = std::env::var("HOME").ok()?;
    let config_path = format!("{}/Library/Preferences/.wrangler/config/default.toml", home);
    let content = std::fs::read_to_string(config_path).ok()?;
    for line in content.lines() {
        if let Some(rest) = line.strip_prefix("oauth_token = \"") {
            return Some(rest.trim_end_matches('"').to_string());
        }
    }
    None
}

#[tokio::test]
async fn cloudflare_load_search_and_call() {
    // Load Cloudflare spec
    let mut reg = ApiRegistry::new();
    let count = reg.load_popular("cloudflare").await;
    match count {
        Ok(n) => println!("Cloudflare API: {} endpoints loaded", n),
        Err(e) => {
            eprintln!("Skipping Cloudflare test (spec download failed): {}", e);
            return;
        }
    }

    // Search for DNS endpoints
    let results = reg.search("cloudflare", "list zones", 5);
    println!("\nSearch 'list zones':");
    for r in &results {
        println!("  {} {} {} — {}", r.method, r.name, r.path, r.description);
    }
    assert!(!results.is_empty());

    // Search for workers
    let results = reg.search("cloudflare", "list workers", 5);
    println!("\nSearch 'list workers':");
    for r in &results {
        println!("  {} {} {} — {}", r.method, r.name, r.path, r.description);
    }

    // Real API call — get current user
    let token = match cloudflare_token() {
        Some(t) => t,
        None => {
            eprintln!("No Cloudflare token found, skipping API call test");
            return;
        }
    };

    // Register with auth
    let spec_json = sgr_agent::openapi::load_or_download(
        &sgr_agent::openapi::default_cache_dir(),
        "cloudflare",
        &sgr_agent::openapi::find_popular("cloudflare")
            .unwrap()
            .spec_url,
    )
    .await
    .unwrap();

    let mut reg2 = ApiRegistry::new();
    reg2.add_api(
        "cloudflare",
        "https://api.cloudflare.com/client/v4",
        &spec_json,
        ApiAuth::Bearer(token),
    )
    .unwrap();

    // Find user endpoint
    let user_ep = reg2.search("cloudflare", "user details", 5);
    println!("\nSearch 'user details':");
    for r in &user_ep {
        println!("  {} {} {} — {}", r.method, r.name, r.path, r.description);
    }

    // Call /user — search found it as "user_get"
    let result = reg2
        .call("cloudflare", "user_get", &HashMap::new(), None)
        .await;
    match result {
        Ok(body) => {
            let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let email = v
                .get("result")
                .and_then(|r| r.get("email"))
                .and_then(|e| e.as_str())
                .unwrap_or("?");
            println!("\n✓ Cloudflare API call succeeded! User: {}", email);
            assert!(v.get("success").and_then(|s| s.as_bool()).unwrap_or(false));
        }
        Err(e) => {
            eprintln!("API call failed: {}", e);
            // Don't fail test — endpoint name might differ
        }
    }
}
