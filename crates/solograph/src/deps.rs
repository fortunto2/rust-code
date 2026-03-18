//! Parse dependency manifests (Cargo.toml, package.json, pyproject.toml).

use std::path::Path;

/// Dependency kind.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DependencyKind {
    Normal,
    Dev,
    Build,
}

/// A parsed dependency.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Dependency {
    pub name: String,
    pub version: String,
    pub kind: DependencyKind,
}

/// Parse dependencies from a manifest file.
///
/// Supported: `Cargo.toml`, `package.json`, `pyproject.toml`.
pub fn parse_deps(path: &Path) -> Vec<Dependency> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec![];
    };

    match name {
        "Cargo.toml" => parse_cargo(&content),
        "package.json" => parse_package_json(&content),
        "pyproject.toml" => parse_pyproject(&content),
        _ => vec![],
    }
}

// ---------------------------------------------------------------------------
// Cargo.toml (simple TOML parsing without full toml crate)
// ---------------------------------------------------------------------------

fn parse_cargo(content: &str) -> Vec<Dependency> {
    let mut deps = Vec::new();
    let mut section = "";

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            section = if trimmed.contains("dev-dependencies") {
                "dev"
            } else if trimmed.contains("build-dependencies") {
                "build"
            } else if trimmed.contains("dependencies") && !trimmed.contains("workspace") {
                "normal"
            } else {
                ""
            };
            continue;
        }

        if section.is_empty() {
            continue;
        }

        // Parse: name = "version" or name = { version = "..." }
        if let Some((name, rest)) = trimmed.split_once('=') {
            let name = name.trim().trim_matches('"');
            if name.is_empty() || name.starts_with('#') {
                continue;
            }

            let rest = rest.trim();
            let version = if rest.starts_with('"') {
                rest.trim_matches('"').to_string()
            } else if rest.starts_with('{') {
                extract_version_from_table(rest)
            } else {
                rest.to_string()
            };

            let kind = match section {
                "dev" => DependencyKind::Dev,
                "build" => DependencyKind::Build,
                _ => DependencyKind::Normal,
            };

            deps.push(Dependency {
                name: name.to_string(),
                version,
                kind,
            });
        }
    }

    deps
}

fn extract_version_from_table(s: &str) -> String {
    // Extract version = "X.Y.Z" from inline table
    if let Some(idx) = s.find("version") {
        let after = &s[idx..];
        if let Some(start) = after.find('"') {
            let rest = &after[start + 1..];
            if let Some(end) = rest.find('"') {
                return rest[..end].to_string();
            }
        }
    }
    // Path or git dependency
    if s.contains("path") {
        return "path".to_string();
    }
    if s.contains("git") {
        return "git".to_string();
    }
    "?".to_string()
}

// ---------------------------------------------------------------------------
// package.json
// ---------------------------------------------------------------------------

fn parse_package_json(content: &str) -> Vec<Dependency> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(content) else {
        return vec![];
    };

    let mut deps = Vec::new();

    if let Some(obj) = val.get("dependencies").and_then(|d| d.as_object()) {
        for (name, ver) in obj {
            deps.push(Dependency {
                name: name.clone(),
                version: ver.as_str().unwrap_or("?").to_string(),
                kind: DependencyKind::Normal,
            });
        }
    }

    if let Some(obj) = val.get("devDependencies").and_then(|d| d.as_object()) {
        for (name, ver) in obj {
            deps.push(Dependency {
                name: name.clone(),
                version: ver.as_str().unwrap_or("?").to_string(),
                kind: DependencyKind::Dev,
            });
        }
    }

    deps
}

// ---------------------------------------------------------------------------
// pyproject.toml (basic parsing)
// ---------------------------------------------------------------------------

fn parse_pyproject(content: &str) -> Vec<Dependency> {
    let mut deps = Vec::new();
    let mut in_deps = false;

    for line in content.lines() {
        let trimmed = line.trim();

        if trimmed == "dependencies = [" || trimmed.starts_with("dependencies = [") {
            in_deps = true;
            // Handle inline: dependencies = ["foo>=1.0", "bar"]
            if trimmed.contains(']') {
                for dep in extract_pyproject_inline(trimmed) {
                    deps.push(dep);
                }
                in_deps = false;
            }
            continue;
        }

        if in_deps {
            if trimmed.starts_with(']') {
                in_deps = false;
                continue;
            }
            let dep_str = trimmed.trim_matches(|c: char| c == '"' || c == '\'' || c == ',');
            if !dep_str.is_empty() {
                let (name, version) = parse_pep508(dep_str);
                deps.push(Dependency {
                    name,
                    version,
                    kind: DependencyKind::Normal,
                });
            }
        }
    }

    deps
}

fn extract_pyproject_inline(line: &str) -> Vec<Dependency> {
    let start = line.find('[').unwrap_or(0) + 1;
    let end = line.find(']').unwrap_or(line.len());
    let inner = &line[start..end];

    inner
        .split(',')
        .filter_map(|s| {
            let s = s.trim().trim_matches(|c: char| c == '"' || c == '\'');
            if s.is_empty() {
                return None;
            }
            let (name, version) = parse_pep508(s);
            Some(Dependency {
                name,
                version,
                kind: DependencyKind::Normal,
            })
        })
        .collect()
}

/// Parse PEP 508: "package>=1.0,<2.0" → ("package", ">=1.0,<2.0")
fn parse_pep508(s: &str) -> (String, String) {
    let split_at = s.find(['>', '<', '=', '!', '~', '[']).unwrap_or(s.len());
    let name = s[..split_at].trim().to_string();
    let version = if split_at < s.len() {
        s[split_at..].trim().to_string()
    } else {
        "*".to_string()
    };
    (name, version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cargo_toml() {
        let content = r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
serde = "1"
tokio = { version = "1", features = ["full"] }
local-crate = { path = "../local" }

[dev-dependencies]
tempfile = "3"
"#;
        let deps = parse_cargo(content);
        assert_eq!(deps.len(), 4);

        let serde = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde.version, "1");
        assert_eq!(serde.kind, DependencyKind::Normal);

        let tokio = deps.iter().find(|d| d.name == "tokio").unwrap();
        assert_eq!(tokio.version, "1");

        let local = deps.iter().find(|d| d.name == "local-crate").unwrap();
        assert_eq!(local.version, "path");

        let tempfile = deps.iter().find(|d| d.name == "tempfile").unwrap();
        assert_eq!(tempfile.kind, DependencyKind::Dev);
    }

    #[test]
    fn parse_pkg_json() {
        let content = r#"{
  "dependencies": {
    "react": "^18.0.0",
    "next": "^14.0.0"
  },
  "devDependencies": {
    "typescript": "^5.0.0"
  }
}"#;
        let deps = parse_package_json(content);
        assert_eq!(deps.len(), 3);
        assert!(
            deps.iter()
                .any(|d| d.name == "react" && d.version == "^18.0.0")
        );
        assert!(
            deps.iter()
                .any(|d| d.name == "typescript" && d.kind == DependencyKind::Dev)
        );
    }

    #[test]
    fn parse_pep508_spec() {
        assert_eq!(
            parse_pep508("requests>=2.28"),
            ("requests".into(), ">=2.28".into())
        );
        assert_eq!(parse_pep508("flask"), ("flask".into(), "*".into()));
        assert_eq!(
            parse_pep508("numpy>=1.24,<2.0"),
            ("numpy".into(), ">=1.24,<2.0".into())
        );
    }
}
