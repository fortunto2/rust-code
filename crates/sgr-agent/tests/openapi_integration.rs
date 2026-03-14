//! Integration test: download real GitHub OpenAPI spec, parse, search.
//!
//! Run: cargo test -p sgr-agent --features "agent search" --test openapi_integration

use sgr_agent::openapi::ApiRegistry;

#[tokio::test]
async fn download_and_search_github_api() {
    let mut reg = ApiRegistry::new();
    let count = reg.load_popular("github").await;

    match count {
        Ok(n) => {
            println!("GitHub API: {} endpoints loaded", n);
            assert!(n > 100, "GitHub API should have 100+ endpoints, got {}", n);

            // Search for issues
            let results = reg.search("github", "create issue", 10);
            println!("\nSearch 'create issue':");
            for r in &results {
                println!("  {} {} {} — {}", r.method, r.name, r.path, r.description);
            }
            assert!(!results.is_empty(), "Should find issue-related endpoints");

            // Search for pull requests
            let results = reg.search("github", "pull request review", 5);
            println!("\nSearch 'pull request review':");
            for r in &results {
                println!("  {} {} {} — {}", r.method, r.name, r.path, r.description);
            }

            // Search for repos
            let results = reg.search("github", "list repositories", 5);
            println!("\nSearch 'list repositories':");
            for r in &results {
                println!("  {} {} {} — {}", r.method, r.name, r.path, r.description);
            }
            assert!(!results.is_empty());

            // Find specific endpoint
            let ep = reg.find_endpoint("github", "repos_owner_repo_get");
            if let Some(ep) = ep {
                println!("\nFound: {} {} — {}", ep.method, ep.path, ep.description);
                println!(
                    "  Params: {:?}",
                    ep.params.iter().map(|p| &p.name).collect::<Vec<_>>()
                );
            }
        }
        Err(e) => {
            // Network might be unavailable in CI
            eprintln!("Skipping GitHub test (network error): {}", e);
        }
    }
}

#[tokio::test]
async fn cache_works_across_calls() {
    let mut reg1 = ApiRegistry::new();
    let r1 = reg1.load_popular("github").await;

    if r1.is_err() {
        eprintln!("Skipping cache test (network error)");
        return;
    }

    // Second load should use cache (fast)
    let mut reg2 = ApiRegistry::new();
    let start = std::time::Instant::now();
    let r2 = reg2.load_popular("github").await;
    let elapsed = start.elapsed();

    assert!(r2.is_ok());
    assert_eq!(r1.unwrap(), r2.unwrap());
    println!("Cached load took: {:?}", elapsed);
    // Cache should be <100ms (vs network ~1-5s)
}
