//! Scan project files via `git ls-files` (or fallback walk).

use std::path::{Path, PathBuf};

/// Known source file extensions → language name.
pub fn language_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "py" => Some("python"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "swift" => Some("swift"),
        "kt" | "kts" => Some("kotlin"),
        "go" => Some("go"),
        "rb" => Some("ruby"),
        "java" => Some("java"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" => Some("cpp"),
        "toml" => Some("toml"),
        "json" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "md" => Some("markdown"),
        "baml" => Some("baml"),
        _ => None,
    }
}

/// Info about a single source file.
#[derive(Debug, Clone)]
pub struct FileInfo {
    pub path: PathBuf,
    pub language: &'static str,
    pub lines: usize,
    pub bytes: usize,
}

/// Aggregate stats for a scanned project.
#[derive(Debug, Default)]
pub struct ProjectStats {
    pub files: Vec<FileInfo>,
    pub total_lines: usize,
    pub total_bytes: usize,
    pub languages: Vec<(String, usize)>, // (language, file_count)
}

/// Scan a project directory for source files.
///
/// Uses `git ls-files` as source of truth (respects .gitignore, excludes
/// nested repos, untracked junk). Falls back to walk_dir if not in a git repo.
pub fn scan_project(root: &Path) -> ProjectStats {
    let mut stats = ProjectStats::default();
    let mut lang_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();

    let files = git_ls_files(root).unwrap_or_else(|| walk_dir_collect(root));

    for path in files {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(language) = language_for_ext(ext) else {
            continue;
        };

        let (lines, bytes) = count_lines(&path);
        *lang_counts.entry(language.to_string()).or_default() += 1;

        stats.files.push(FileInfo {
            path,
            language,
            lines,
            bytes,
        });
    }

    for fi in &stats.files {
        stats.total_lines += fi.lines;
        stats.total_bytes += fi.bytes;
    }

    let mut langs: Vec<_> = lang_counts.into_iter().collect();
    langs.sort_by(|a, b| b.1.cmp(&a.1));
    stats.languages = langs;

    stats
}

/// Get tracked files from git. Returns None if not a git repo.
fn git_ls_files(root: &Path) -> Option<Vec<PathBuf>> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<PathBuf> = stdout
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| root.join(s))
        .collect();

    Some(files)
}

/// Fallback: walk directories manually (for non-git projects).
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "baml_client",
];

fn walk_dir_collect(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_dir(root, &mut files);
    files
}

fn walk_dir(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();

        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if SKIP_DIRS.contains(&name) || name.starts_with('.') {
                continue;
            }
            if path.join(".git").exists() {
                continue;
            }
            walk_dir(&path, files);
            continue;
        }

        files.push(path);
    }
}

fn count_lines(path: &Path) -> (usize, usize) {
    match std::fs::read(path) {
        Ok(data) => {
            let lines = data.iter().filter(|&&b| b == b'\n').count();
            (lines.max(1), data.len())
        }
        Err(_) => (0, 0),
    }
}

/// Build a compact directory tree from scanned files.
///
/// Groups files by directory, shows file count and LOC per dir.
pub fn dir_tree(root: &Path, files: &[FileInfo]) -> String {
    use std::collections::BTreeMap;

    let mut dirs: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for fi in files {
        let rel = fi.path.strip_prefix(root).unwrap_or(&fi.path);
        let dir = rel
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(".")
            .to_string();
        let entry = dirs.entry(dir).or_default();
        entry.0 += 1;
        entry.1 += fi.lines;
    }

    let mut out = String::new();
    for (dir, (count, lines)) in &dirs {
        let display = if dir.is_empty() { "." } else { dir.as_str() };
        out.push_str(&format!(
            "  {}/ ({} files, {} lines)\n",
            display, count, lines
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_detection() {
        assert_eq!(language_for_ext("rs"), Some("rust"));
        assert_eq!(language_for_ext("py"), Some("python"));
        assert_eq!(language_for_ext("tsx"), Some("typescript"));
        assert_eq!(language_for_ext("unknown"), None);
    }

    #[test]
    fn scan_own_project() {
        let stats = scan_project(Path::new("."));
        assert!(!stats.files.is_empty());
        assert!(stats.total_lines > 0);
        assert!(stats.languages.iter().any(|(l, _)| l == "rust"));
    }

    #[test]
    fn git_ls_files_works() {
        // We're in a git repo, so this should return files
        let files = git_ls_files(Path::new("."));
        assert!(files.is_some());
        let files = files.unwrap();
        assert!(!files.is_empty());
        // Should contain our own source files
        assert!(
            files
                .iter()
                .any(|p| p.to_str().unwrap().contains("scanner.rs"))
        );
    }
}
