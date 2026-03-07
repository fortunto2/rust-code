//! Walk project directories, find source files, compute stats.

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

/// Directories to always skip.
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

/// Scan a project directory for source files.
///
/// Walks recursively, skips common build/vendor dirs, counts lines.
pub fn scan_project(root: &Path) -> ProjectStats {
    let mut stats = ProjectStats::default();
    let mut lang_counts = std::collections::HashMap::new();
    walk_dir(root, &mut stats.files, &mut lang_counts);

    for fi in &stats.files {
        stats.total_lines += fi.lines;
        stats.total_bytes += fi.bytes;
    }

    let mut langs: Vec<_> = lang_counts.into_iter().collect();
    langs.sort_by(|a, b| b.1.cmp(&a.1));
    stats.languages = langs;

    stats
}

fn walk_dir(
    dir: &Path,
    files: &mut Vec<FileInfo>,
    lang_counts: &mut std::collections::HashMap<String, usize>,
) {
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
            // Skip nested git repos (they're separate projects)
            if path.join(".git").exists() {
                continue;
            }
            walk_dir(&path, files, lang_counts);
            continue;
        }

        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Some(language) = language_for_ext(ext) else {
            continue;
        };

        let (lines, bytes) = count_lines(&path);
        *lang_counts.entry(language.to_string()).or_default() += 1;

        files.push(FileInfo {
            path,
            language,
            lines,
            bytes,
        });
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
/// Output like:
/// ```text
///   crates/rc-cli/src/ (12 files, 4200 lines)
///   crates/baml-agent/src/ (8 files, 2100 lines)
/// ```
pub fn dir_tree(root: &Path, files: &[FileInfo]) -> String {
    use std::collections::BTreeMap;

    // Aggregate per directory
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
        let stats = scan_project(Path::new("src"));
        assert!(!stats.files.is_empty());
        assert!(stats.total_lines > 0);
        assert!(stats.languages.iter().any(|(l, _)| l == "rust"));
    }
}
