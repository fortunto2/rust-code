//! Load, cache, and merge prompt files from disk.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

/// Loads and caches prompt files from a directory.
pub struct PromptLoader {
    base_dir: PathBuf,
    cache: RwLock<HashMap<String, String>>,
}

impl PromptLoader {
    /// Create a new prompt loader rooted at the given directory.
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Load a prompt file by relative path (e.g., "system.md", "roles/explorer.md").
    /// Returns cached version if already loaded.
    pub fn load(&self, relative_path: &str) -> Result<String, PromptError> {
        // Check cache first
        if let Ok(cache) = self.cache.read()
            && let Some(cached) = cache.get(relative_path)
        {
            return Ok(cached.clone());
        }

        let full_path = self.base_dir.join(relative_path);
        let content = std::fs::read_to_string(&full_path)
            .map_err(|e| PromptError::Io(full_path.clone(), e))?;

        // Process includes: {{include:path/to/file.md}}
        let processed = self.process_includes(&content, 0)?;

        if let Ok(mut cache) = self.cache.write() {
            cache.insert(relative_path.to_string(), processed.clone());
        }

        Ok(processed)
    }

    /// Load and merge multiple prompt files, separated by newlines.
    pub fn load_merged(&self, paths: &[&str]) -> Result<String, PromptError> {
        let mut parts = Vec::new();
        for path in paths {
            parts.push(self.load(path)?);
        }
        Ok(parts.join("\n\n"))
    }

    /// Load a prompt with variable substitution.
    /// Variables are {{key}} patterns in the template.
    pub fn load_with_vars(
        &self,
        path: &str,
        vars: &HashMap<String, String>,
    ) -> Result<String, PromptError> {
        let mut content = self.load(path)?;
        for (key, value) in vars {
            content = content.replace(&format!("{{{{{}}}}}", key), value);
        }
        Ok(content)
    }

    /// Clear the cache, forcing reload on next access.
    pub fn clear_cache(&self) {
        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
        }
    }

    /// Return the base directory this loader reads from.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Process {{include:path}} directives recursively (max depth 5).
    /// Include paths are canonicalized to prevent directory traversal attacks.
    fn process_includes(&self, content: &str, depth: usize) -> Result<String, PromptError> {
        if depth > 5 {
            return Err(PromptError::MaxIncludeDepth);
        }

        let mut result = String::with_capacity(content.len());
        let mut remaining = content;

        while let Some(start) = remaining.find("{{include:") {
            result.push_str(&remaining[..start]);
            let after_tag = &remaining[start + 10..];
            if let Some(end) = after_tag.find("}}") {
                let include_path = &after_tag[..end];
                let full_path = self.base_dir.join(include_path);
                // Canonicalize to prevent path traversal (../ and symlinks)
                let canonical = std::fs::canonicalize(&full_path)
                    .map_err(|e| PromptError::Io(full_path.clone(), e))?;
                let canonical_base = std::fs::canonicalize(&self.base_dir)
                    .map_err(|e| PromptError::Io(self.base_dir.clone(), e))?;
                if !canonical.starts_with(&canonical_base) {
                    return Err(PromptError::PathTraversal(include_path.to_string()));
                }
                let included = std::fs::read_to_string(&canonical)
                    .map_err(|e| PromptError::Io(full_path, e))?;
                let processed = self.process_includes(&included, depth + 1)?;
                result.push_str(&processed);
                remaining = &after_tag[end + 2..];
            } else {
                result.push_str("{{include:");
                remaining = after_tag;
            }
        }
        result.push_str(remaining);

        Ok(result)
    }
}

/// Errors from prompt loading.
#[derive(Debug)]
pub enum PromptError {
    /// File I/O error.
    Io(PathBuf, std::io::Error),
    /// Too many nested includes.
    MaxIncludeDepth,
    /// Include path escapes base directory.
    PathTraversal(String),
}

impl std::fmt::Display for PromptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(path, e) => write!(f, "Failed to load prompt '{}': {}", path.display(), e),
            Self::MaxIncludeDepth => write!(f, "Maximum include depth (5) exceeded"),
            Self::PathTraversal(path) => {
                write!(f, "Include path '{}' escapes base directory", path)
            }
        }
    }
}

impl std::error::Error for PromptError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("system.md"),
            "You are an agent.\n\nBe helpful.",
        )
        .unwrap();
        fs::write(dir.path().join("mode.md"), "Mode: execute").unwrap();

        fs::create_dir_all(dir.path().join("roles")).unwrap();
        fs::write(
            dir.path().join("roles/explorer.md"),
            "You are an explorer. Read-only.",
        )
        .unwrap();

        // Include test
        fs::write(
            dir.path().join("with_include.md"),
            "Header\n{{include:roles/explorer.md}}\nFooter",
        )
        .unwrap();

        dir
    }

    #[test]
    fn load_basic() {
        let dir = setup_test_dir();
        let loader = PromptLoader::new(dir.path());
        let content = loader.load("system.md").unwrap();
        assert!(content.contains("You are an agent"));
    }

    #[test]
    fn load_cached() {
        let dir = setup_test_dir();
        let loader = PromptLoader::new(dir.path());
        let _ = loader.load("system.md").unwrap();
        // Second load should use cache
        let content = loader.load("system.md").unwrap();
        assert!(content.contains("You are an agent"));
    }

    #[test]
    fn load_merged() {
        let dir = setup_test_dir();
        let loader = PromptLoader::new(dir.path());
        let content = loader.load_merged(&["system.md", "mode.md"]).unwrap();
        assert!(content.contains("You are an agent"));
        assert!(content.contains("Mode: execute"));
    }

    #[test]
    fn load_with_vars() {
        let dir = setup_test_dir();
        fs::write(
            dir.path().join("template.md"),
            "Hello {{name}}, you are {{role}}.",
        )
        .unwrap();
        let loader = PromptLoader::new(dir.path());
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), "Agent-1".to_string());
        vars.insert("role".to_string(), "explorer".to_string());
        let content = loader.load_with_vars("template.md", &vars).unwrap();
        assert_eq!(content, "Hello Agent-1, you are explorer.");
    }

    #[test]
    fn load_with_includes() {
        let dir = setup_test_dir();
        let loader = PromptLoader::new(dir.path());
        let content = loader.load("with_include.md").unwrap();
        assert!(content.contains("Header"));
        assert!(content.contains("You are an explorer"));
        assert!(content.contains("Footer"));
    }

    #[test]
    fn load_missing_file() {
        let dir = setup_test_dir();
        let loader = PromptLoader::new(dir.path());
        assert!(loader.load("nonexistent.md").is_err());
    }

    #[test]
    fn include_path_traversal_blocked() {
        let dir = setup_test_dir();
        // Create a file that tries to include outside base_dir
        fs::write(
            dir.path().join("evil.md"),
            "Before\n{{include:../../../etc/hostname}}\nAfter",
        )
        .unwrap();
        let loader = PromptLoader::new(dir.path());
        let result = loader.load("evil.md");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("escapes base directory") || err.contains("Failed to load"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn clear_cache_works() {
        let dir = setup_test_dir();
        let loader = PromptLoader::new(dir.path());
        let _ = loader.load("system.md").unwrap();
        loader.clear_cache();
        // Should reload from disk
        let content = loader.load("system.md").unwrap();
        assert!(content.contains("You are an agent"));
    }
}
