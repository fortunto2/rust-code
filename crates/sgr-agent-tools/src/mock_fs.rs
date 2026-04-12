//! MockFs — in-memory `FileBackend` for testing.
//!
//! No disk I/O, fully deterministic, instant.
//!
//! ```rust,ignore
//! use sgr_agent_tools::{MockFs, ReadTool, WriteTool};
//!
//! let fs = Arc::new(MockFs::new());
//! fs.add_file("readme.md", "# Hello");
//! fs.add_file("src/main.rs", "fn main() {}");
//!
//! let read = ReadTool(fs.clone());
//! let write = WriteTool(fs.clone());
//! ```

use std::collections::BTreeMap;
use std::sync::RwLock;

use anyhow::{Result, bail};

use sgr_agent_core::backend::FileBackend;

/// In-memory filesystem for testing. Thread-safe via RwLock.
pub struct MockFs {
    files: RwLock<BTreeMap<String, String>>,
    context_value: RwLock<String>,
}

impl MockFs {
    pub fn new() -> Self {
        Self {
            files: RwLock::new(BTreeMap::new()),
            context_value: RwLock::new("2026-01-15 10:00:00".to_string()),
        }
    }

    /// Pre-populate a file.
    pub fn add_file(&self, path: &str, content: &str) {
        self.files
            .write()
            .unwrap()
            .insert(normalize(path), content.to_string());
    }

    /// Set what context() returns.
    pub fn set_context(&self, value: &str) {
        *self.context_value.write().unwrap() = value.to_string();
    }

    /// Get all files as snapshot (for assertions).
    pub fn snapshot(&self) -> BTreeMap<String, String> {
        self.files.read().unwrap().clone()
    }

    /// Check if file exists.
    pub fn exists(&self, path: &str) -> bool {
        self.files.read().unwrap().contains_key(&normalize(path))
    }

    /// Get file content (for assertions).
    pub fn content(&self, path: &str) -> Option<String> {
        self.files.read().unwrap().get(&normalize(path)).cloned()
    }
}

impl Default for MockFs {
    fn default() -> Self {
        Self::new()
    }
}

fn normalize(path: &str) -> String {
    path.trim_start_matches('/').to_string()
}

#[async_trait::async_trait]
impl FileBackend for MockFs {
    async fn read(
        &self,
        path: &str,
        number: bool,
        start_line: i32,
        end_line: i32,
    ) -> Result<String> {
        let files = self.files.read().unwrap();
        let content = files
            .get(&normalize(path))
            .ok_or_else(|| anyhow::anyhow!("file not found: {path}"))?;

        let lines: Vec<&str> = content.lines().collect();
        let start = if start_line > 0 {
            (start_line - 1) as usize
        } else {
            0
        };
        let end = if end_line > 0 {
            (end_line as usize).min(lines.len())
        } else {
            lines.len()
        };

        let mut out = String::new();
        for (i, line) in lines[start..end].iter().enumerate() {
            if number {
                use std::fmt::Write;
                let _ = write!(out, "{}\t{}\n", start + i + 1, line);
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        Ok(out)
    }

    async fn write(&self, path: &str, content: &str, start_line: i32, end_line: i32) -> Result<()> {
        let key = normalize(path);
        let mut files = self.files.write().unwrap();

        if start_line > 0 && end_line > 0 {
            let existing = files.get(&key).cloned().unwrap_or_default();
            let mut lines: Vec<&str> = existing.lines().collect();
            let start = (start_line - 1) as usize;
            let end = (end_line as usize).min(lines.len());
            let new_lines: Vec<&str> = content.lines().collect();
            lines.splice(start..end, new_lines);
            files.insert(key, lines.join("\n") + "\n");
        } else {
            files.insert(key, content.to_string());
        }
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let key = normalize(path);
        let mut files = self.files.write().unwrap();
        if files.remove(&key).is_none() {
            bail!("file not found: {path}");
        }
        Ok(())
    }

    async fn search(&self, root: &str, pattern: &str, limit: i32) -> Result<String> {
        let re = regex::Regex::new(pattern)?;
        let files = self.files.read().unwrap();
        let root_norm = normalize(root);
        let max = if limit > 0 { limit as usize } else { 500 };

        let mut out = String::new();
        let mut count = 0;
        for (path, content) in files.iter() {
            if !root_norm.is_empty() && root_norm != "/" && !path.starts_with(&root_norm) {
                continue;
            }
            for (i, line) in content.lines().enumerate() {
                if count >= max {
                    return Ok(out);
                }
                if re.is_match(line) {
                    use std::fmt::Write;
                    let _ = write!(out, "{}:{}:{}\n", path, i + 1, line);
                    count += 1;
                }
            }
        }
        Ok(out)
    }

    async fn list(&self, path: &str) -> Result<String> {
        let files = self.files.read().unwrap();
        let prefix = normalize(path);
        let prefix = if prefix.is_empty() || prefix == "/" {
            String::new()
        } else {
            format!("{prefix}/")
        };

        let mut entries = std::collections::BTreeSet::new();
        for key in files.keys() {
            if let Some(rest) = key.strip_prefix(&prefix) {
                if let Some(slash) = rest.find('/') {
                    entries.insert(format!("{}/", &rest[..slash]));
                } else {
                    entries.insert(rest.to_string());
                }
            } else if prefix.is_empty() {
                if let Some(slash) = key.find('/') {
                    entries.insert(format!("{}/", &key[..slash]));
                } else {
                    entries.insert(key.clone());
                }
            }
        }

        let mut out = format!("$ ls {path}\n");
        for entry in entries {
            out.push_str(&entry);
            out.push('\n');
        }
        Ok(out)
    }

    async fn tree(&self, _root: &str, _level: i32) -> Result<String> {
        let files = self.files.read().unwrap();
        let mut out = String::new();
        for key in files.keys() {
            out.push_str(key);
            out.push('\n');
        }
        Ok(out)
    }

    async fn context(&self) -> Result<String> {
        Ok(self.context_value.read().unwrap().clone())
    }

    async fn mkdir(&self, _path: &str) -> Result<()> {
        Ok(()) // directories are implicit in mock
    }

    async fn move_file(&self, from: &str, to: &str) -> Result<()> {
        let mut files = self.files.write().unwrap();
        let content = files
            .remove(&normalize(from))
            .ok_or_else(|| anyhow::anyhow!("file not found: {from}"))?;
        files.insert(normalize(to), content);
        Ok(())
    }

    async fn find(&self, root: &str, name: &str, file_type: &str, _limit: i32) -> Result<String> {
        let files = self.files.read().unwrap();
        let root_norm = normalize(root);
        let _ = file_type; // mock doesn't distinguish files/dirs

        let results: Vec<&str> = files
            .keys()
            .filter(|k| {
                (root_norm.is_empty() || root_norm == "/" || k.starts_with(&root_norm))
                    && k.contains(name)
            })
            .map(|k| k.as_str())
            .collect();
        Ok(results.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn basic_crud() {
        let fs = MockFs::new();
        fs.add_file("hello.txt", "world");

        let content = fs.read("hello.txt", false, 0, 0).await.unwrap();
        assert_eq!(content.trim(), "world");

        fs.write("hello.txt", "updated", 0, 0).await.unwrap();
        assert_eq!(fs.content("hello.txt").unwrap(), "updated");

        fs.delete("hello.txt").await.unwrap();
        assert!(!fs.exists("hello.txt"));
    }

    #[tokio::test]
    async fn search_mock() {
        let fs = MockFs::new();
        fs.add_file("a.txt", "hello world\nfoo bar");
        fs.add_file("b.txt", "baz qux");

        let results = fs.search("/", "hello", 10).await.unwrap();
        assert!(results.contains("a.txt:1:hello world"));
        assert!(!results.contains("b.txt"));
    }

    #[tokio::test]
    async fn list_mock() {
        let fs = MockFs::new();
        fs.add_file("readme.md", "hi");
        fs.add_file("src/main.rs", "fn main() {}");
        fs.add_file("src/lib.rs", "pub mod foo;");

        let listing = fs.list("/").await.unwrap();
        assert!(listing.contains("readme.md"));
        assert!(listing.contains("src/"));

        let src_listing = fs.list("src").await.unwrap();
        assert!(src_listing.contains("main.rs"));
        assert!(src_listing.contains("lib.rs"));
    }

    #[tokio::test]
    async fn move_file_mock() {
        let fs = MockFs::new();
        fs.add_file("old.txt", "data");

        fs.move_file("old.txt", "new.txt").await.unwrap();
        assert!(!fs.exists("old.txt"));
        assert_eq!(fs.content("new.txt").unwrap(), "data");
    }

    #[tokio::test]
    async fn ranged_read() {
        let fs = MockFs::new();
        fs.add_file("test.txt", "line1\nline2\nline3\nline4");

        let range = fs.read("test.txt", true, 2, 3).await.unwrap();
        assert!(range.contains("2\tline2"));
        assert!(range.contains("3\tline3"));
        assert!(!range.contains("line1"));
        assert!(!range.contains("line4"));
    }

    #[tokio::test]
    async fn context_mock() {
        let fs = MockFs::new();
        assert!(fs.context().await.unwrap().contains("2026"));

        fs.set_context("2030-12-25 00:00:00");
        assert!(fs.context().await.unwrap().contains("2030"));
    }

    #[tokio::test]
    async fn snapshot() {
        let fs = MockFs::new();
        fs.add_file("a.txt", "1");
        fs.add_file("b.txt", "2");

        let snap = fs.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap["a.txt"], "1");
    }
}
