//! LocalFs — ready-to-use `FileBackend` for local filesystem via std::fs.
//!
//! Requires feature `local-fs`.
//!
//! ```rust,ignore
//! use sgr_agent_tools::{LocalFs, ReadTool, SearchTool};
//! let fs = Arc::new(LocalFs::new("/path/to/workspace"));
//! let read = ReadTool(fs.clone());
//! ```

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::backend::FileBackend;

/// Known binary extensions — skip during search to avoid wasted I/O.
const BINARY_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "svg", "mp4", "mov", "avi", "mkv", "mp3",
    "wav", "flac", "ogg", "zip", "tar", "gz", "bz2", "xz", "7z", "rar", "pdf", "doc", "docx",
    "xls", "xlsx", "ppt", "pptx", "onnx", "bin", "wasm", "so", "dylib", "dll", "exe", "o", "a",
    "pyc", "class", "jar", "db", "sqlite", "sqlite3",
];

/// Max recursion depth for find (prevent runaway on deeply nested dirs).
const MAX_FIND_DEPTH: usize = 20;

/// Local filesystem backend rooted at a workspace directory.
///
/// All paths are resolved relative to `root`. Path traversal (`..`) and symlink
/// escapes are blocked.
pub struct LocalFs {
    root: PathBuf,
}

impl LocalFs {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Resolve a workspace-relative path to absolute, preventing traversal outside root.
    /// Checks both literal `..` and post-canonicalization containment (symlink safety).
    fn resolve(&self, path: &str) -> Result<PathBuf> {
        let path = path.trim_start_matches('/');
        if path.contains("..") {
            bail!("path traversal blocked: {path}");
        }
        let full = self.root.join(path);
        // For existing paths, canonicalize and verify containment (catches symlinks).
        // For new paths (write/mkdir), canonicalize parent.
        let canonical = std::fs::canonicalize(&full).or_else(|_| {
            if let Some(parent) = full.parent() {
                std::fs::canonicalize(parent).map(|p| p.join(full.file_name().unwrap_or_default()))
            } else {
                Ok(full.clone())
            }
        }).unwrap_or_else(|_| full.clone());
        let root_canonical = std::fs::canonicalize(&self.root).unwrap_or_else(|_| self.root.clone());
        if !canonical.starts_with(&root_canonical) {
            bail!("path escapes workspace root: {path}");
        }
        Ok(full)
    }
}

#[async_trait::async_trait]
impl FileBackend for LocalFs {
    async fn read(&self, path: &str, number: bool, start_line: i32, end_line: i32) -> Result<String> {
        let full = self.resolve(path)?;
        let content = tokio::fs::read_to_string(&full)
            .await
            .with_context(|| format!("read {}", full.display()))?;

        let lines: Vec<&str> = content.lines().collect();
        let start = if start_line > 0 { (start_line - 1) as usize } else { 0 };
        let end = if end_line > 0 { end_line as usize } else { lines.len() };
        let end = end.min(lines.len());

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
        let full = self.resolve(path)?;
        if let Some(parent) = full.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        if start_line > 0 && end_line > 0 {
            let existing = tokio::fs::read_to_string(&full).await.unwrap_or_default();
            let mut lines: Vec<&str> = existing.lines().collect();
            let start = (start_line - 1) as usize;
            let end = (end_line as usize).min(lines.len());
            let new_lines: Vec<&str> = content.lines().collect();
            lines.splice(start..end, new_lines);
            tokio::fs::write(&full, lines.join("\n") + "\n").await?;
        } else {
            tokio::fs::write(&full, content).await?;
        }
        Ok(())
    }

    async fn delete(&self, path: &str) -> Result<()> {
        let full = self.resolve(path)?;
        tokio::fs::remove_file(&full)
            .await
            .with_context(|| format!("delete {}", full.display()))
    }

    async fn search(&self, root: &str, pattern: &str, limit: i32) -> Result<String> {
        let dir = self.resolve(root)?;
        let re = regex::Regex::new(pattern).with_context(|| format!("invalid regex: {pattern}"))?;
        let max = if limit > 0 { limit as usize } else { 500 };
        let ws_root = self.root.clone();

        tokio::task::spawn_blocking(move || {
            let mut results = String::new();
            let mut count = 0;
            search_dir_recursive(&dir, &ws_root, &re, max, &mut count, &mut results)?;
            Ok(results)
        })
        .await?
    }

    async fn list(&self, path: &str) -> Result<String> {
        let dir = self.resolve(path)?;
        let mut entries = tokio::fs::read_dir(&dir)
            .await
            .with_context(|| format!("list {}", dir.display()))?;

        let mut out = format!("$ ls {path}\n");
        let mut names: Vec<String> = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                names.push(format!("{name}/"));
            } else {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            out.push_str(&name);
            out.push('\n');
        }
        Ok(out)
    }

    async fn tree(&self, root: &str, level: i32) -> Result<String> {
        let dir = self.resolve(root)?;
        let ws_root = self.root.clone();
        let max_depth = level as usize;

        tokio::task::spawn_blocking(move || {
            let mut out = String::new();
            tree_recursive(&dir, &ws_root, "", max_depth, 0, &mut out)?;
            Ok(out)
        })
        .await?
    }

    async fn context(&self) -> Result<String> {
        Ok(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string())
    }

    async fn mkdir(&self, path: &str) -> Result<()> {
        let full = self.resolve(path)?;
        tokio::fs::create_dir_all(&full)
            .await
            .with_context(|| format!("mkdir {}", full.display()))
    }

    async fn move_file(&self, from: &str, to: &str) -> Result<()> {
        let src = self.resolve(from)?;
        let dst = self.resolve(to)?;
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::rename(&src, &dst)
            .await
            .with_context(|| format!("move {} -> {}", src.display(), dst.display()))
    }

    async fn find(&self, root: &str, name: &str, file_type: &str, limit: i32) -> Result<String> {
        let dir = self.resolve(root)?;
        let max = if limit > 0 { limit as usize } else { 100 };
        let ws_root = self.root.clone();
        let name = name.to_string();
        let file_type = file_type.to_string();

        tokio::task::spawn_blocking(move || {
            let mut results = Vec::new();
            find_recursive(&dir, &ws_root, &name, &file_type, max, MAX_FIND_DEPTH, 0, &mut results)?;
            Ok(results.join("\n"))
        })
        .await?
    }
}

/// Read directory entries, skipping hidden (dot-prefixed) entries.
fn read_dir_visible(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    Ok(std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .collect())
}

/// Check if file extension is known binary.
fn is_binary_ext(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| BINARY_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Recursive grep: search files for regex matches using BufReader (no full-file read).
fn search_dir_recursive(
    dir: &Path,
    root: &Path,
    re: &regex::Regex,
    max: usize,
    count: &mut usize,
    out: &mut String,
) -> Result<()> {
    for entry in read_dir_visible(dir)? {
        if *count >= max { return Ok(()); }
        let path = entry.path();
        if path.is_dir() {
            search_dir_recursive(&path, root, re, max, count, out)?;
        } else if path.is_file() && !is_binary_ext(&path) {
            if let Ok(file) = std::fs::File::open(&path) {
                let reader = BufReader::new(file);
                let rel = path.strip_prefix(root).unwrap_or(&path);
                for (i, line_result) in reader.lines().enumerate() {
                    if *count >= max { return Ok(()); }
                    let Ok(line) = line_result else { break }; // non-UTF8 → likely binary, stop
                    if re.is_match(&line) {
                        use std::fmt::Write;
                        let _ = write!(out, "{}:{}:{}\n", rel.display(), i + 1, line);
                        *count += 1;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Recursive tree: build directory tree string.
fn tree_recursive(
    dir: &Path,
    root: &Path,
    prefix: &str,
    max_depth: usize,
    depth: usize,
    out: &mut String,
) -> Result<()> {
    if max_depth > 0 && depth >= max_depth { return Ok(()); }

    let mut entries = read_dir_visible(dir)?;
    entries.sort_by_key(|e| e.file_name());

    for (i, entry) in entries.iter().enumerate() {
        let is_last = i == entries.len() - 1;
        let connector = if is_last { "└── " } else { "├── " };
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

        if depth == 0 && i == 0 {
            let rel = dir.strip_prefix(root).unwrap_or(dir);
            use std::fmt::Write;
            let _ = write!(out, "{}/\n", rel.display());
        }

        use std::fmt::Write;
        let _ = write!(out, "{prefix}{connector}{}{}\n", name, if is_dir { "/" } else { "" });

        if is_dir {
            let child_prefix = format!("{}{}", prefix, if is_last { "    " } else { "│   " });
            tree_recursive(&entry.path(), root, &child_prefix, max_depth, depth + 1, out)?;
        }
    }
    Ok(())
}

/// Recursive find: match files/dirs by name pattern, with depth limit.
fn find_recursive(
    dir: &Path,
    root: &Path,
    pattern: &str,
    file_type: &str,
    max: usize,
    max_depth: usize,
    depth: usize,
    results: &mut Vec<String>,
) -> Result<()> {
    if max_depth > 0 && depth >= max_depth { return Ok(()); }

    for entry in read_dir_visible(dir)? {
        if results.len() >= max { return Ok(()); }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);

        let type_match = match file_type {
            "files" | "f" => !is_dir,
            "dirs" | "d" => is_dir,
            _ => true,
        };

        if type_match && name.contains(pattern) {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            results.push(rel.display().to_string());
        }

        if is_dir {
            find_recursive(&path, root, pattern, file_type, max, max_depth, depth + 1, results)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_write_delete() {
        let tmp = std::env::temp_dir().join("sgr_localfs_test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let fs = LocalFs::new(&tmp);
        fs.write("test.txt", "line1\nline2\nline3\n", 0, 0).await.unwrap();

        let content = fs.read("test.txt", false, 0, 0).await.unwrap();
        assert!(content.contains("line1"));
        assert!(content.contains("line3"));

        let numbered = fs.read("test.txt", true, 0, 0).await.unwrap();
        assert!(numbered.contains("1\tline1"));

        let range = fs.read("test.txt", false, 2, 2).await.unwrap();
        assert!(range.contains("line2"));
        assert!(!range.contains("line1"));

        fs.delete("test.txt").await.unwrap();
        assert!(fs.read("test.txt", false, 0, 0).await.is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn list_and_tree() {
        let tmp = std::env::temp_dir().join("sgr_localfs_test2");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("a.txt"), "hello").unwrap();
        std::fs::write(tmp.join("sub/b.txt"), "world").unwrap();

        let fs = LocalFs::new(&tmp);

        let listing = fs.list("/").await.unwrap();
        assert!(listing.contains("a.txt"));
        assert!(listing.contains("sub/"));

        let tree = fs.tree("/", 2).await.unwrap();
        assert!(tree.contains("a.txt"));
        assert!(tree.contains("sub/"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn search_files() {
        let tmp = std::env::temp_dir().join("sgr_localfs_test3");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a.txt"), "hello world\nfoo bar").unwrap();
        std::fs::write(tmp.join("b.txt"), "baz qux").unwrap();

        let fs = LocalFs::new(&tmp);
        let results = fs.search("/", "hello", 10).await.unwrap();
        assert!(results.contains("a.txt:1:hello world"));
        assert!(!results.contains("b.txt"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn path_traversal_blocked() {
        let tmp = std::env::temp_dir().join("sgr_localfs_test4");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let fs = LocalFs::new(&tmp);
        assert!(fs.read("../etc/passwd", false, 0, 0).await.is_err());
        assert!(fs.write("../../evil.txt", "pwned", 0, 0).await.is_err());
    }

    #[tokio::test]
    async fn find_files() {
        let tmp = std::env::temp_dir().join("sgr_localfs_test5");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        std::fs::write(tmp.join("readme.md"), "hi").unwrap();
        std::fs::write(tmp.join("sub/readme.md"), "hi").unwrap();

        let fs = LocalFs::new(&tmp);
        let found = fs.find("/", "readme", "", 10).await.unwrap();
        assert!(found.contains("readme.md"));
        assert!(found.lines().count() >= 2);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn mkdir_and_move() {
        let tmp = std::env::temp_dir().join("sgr_localfs_test6");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let fs = LocalFs::new(&tmp);
        fs.write("orig.txt", "data", 0, 0).await.unwrap();
        fs.mkdir("newdir").await.unwrap();
        fs.move_file("orig.txt", "newdir/moved.txt").await.unwrap();

        assert!(fs.read("orig.txt", false, 0, 0).await.is_err());
        let content = fs.read("newdir/moved.txt", false, 0, 0).await.unwrap();
        assert!(content.contains("data"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn search_skips_binary() {
        let tmp = std::env::temp_dir().join("sgr_localfs_test7");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("data.txt"), "needle here").unwrap();
        std::fs::write(tmp.join("image.png"), "needle hidden in binary").unwrap();

        let fs = LocalFs::new(&tmp);
        let results = fs.search("/", "needle", 10).await.unwrap();
        assert!(results.contains("data.txt"));
        assert!(!results.contains("image.png"));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn hidden_dirs_skipped() {
        let tmp = std::env::temp_dir().join("sgr_localfs_test8");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".hidden")).unwrap();
        std::fs::create_dir_all(tmp.join("visible")).unwrap();
        std::fs::write(tmp.join(".hidden/secret.txt"), "secret").unwrap();
        std::fs::write(tmp.join("visible/public.txt"), "public").unwrap();

        let fs = LocalFs::new(&tmp);

        // search skips hidden
        let results = fs.search("/", ".", 100).await.unwrap();
        assert!(!results.contains(".hidden"));
        assert!(results.contains("visible"));

        // find skips hidden
        let found = fs.find("/", "txt", "", 100).await.unwrap();
        assert!(!found.contains(".hidden"));
        assert!(found.contains("visible"));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
