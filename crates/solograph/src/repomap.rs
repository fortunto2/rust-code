//! Generate a project map — concise summary of files and key symbols.
//!
//! Similar to `codegraph_repomap` from SoloGraph MCP but runs locally via tree-sitter.

use std::path::Path;

use crate::scanner::{scan_project, ProjectStats};
use crate::symbols::extract_symbols;

/// Generate a text repomap for a project directory.
///
/// Returns a structured text summary: files grouped by directory,
/// with key public symbols listed under each file.
pub fn generate_repomap(root: &Path) -> String {
    let stats = scan_project(root);
    format_repomap(root, &stats)
}

fn format_repomap(root: &Path, stats: &ProjectStats) -> String {
    let mut output = String::new();
    output.push_str(&format!(
        "# Project Map: {}\n",
        root.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("project")
    ));
    output.push_str(&format!(
        "# {} files, {} lines, languages: {}\n\n",
        stats.files.len(),
        stats.total_lines,
        stats
            .languages
            .iter()
            .map(|(l, c)| format!("{}({})", l, c))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // Group files by parent directory
    let mut dirs: std::collections::BTreeMap<String, Vec<&crate::scanner::FileInfo>> =
        std::collections::BTreeMap::new();

    for fi in &stats.files {
        let rel = fi.path.strip_prefix(root).unwrap_or(&fi.path);
        let dir = rel
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or(".")
            .to_string();
        dirs.entry(dir).or_default().push(fi);
    }

    for (dir, files) in &dirs {
        output.push_str(&format!("## {}/\n", dir));

        for fi in files {
            let rel = fi.path.strip_prefix(root).unwrap_or(&fi.path);
            let filename = rel.to_str().unwrap_or("?");
            output.push_str(&format!("  {} ({} lines)\n", filename, fi.lines));

            // Extract symbols for parseable files
            if matches!(fi.language, "rust" | "python" | "typescript") {
                if let Ok(source) = std::fs::read_to_string(&fi.path) {
                    let symbols = extract_symbols(&fi.path, &source);
                    let public_symbols: Vec<_> = symbols.iter().filter(|s| s.public).collect();

                    if !public_symbols.is_empty() {
                        for sym in public_symbols.iter().take(15) {
                            output.push_str(&format!(
                                "    {:?} {} (L{})\n",
                                sym.kind, sym.name, sym.line
                            ));
                        }
                        if public_symbols.len() > 15 {
                            output.push_str(&format!(
                                "    ... +{} more\n",
                                public_symbols.len() - 15
                            ));
                        }
                    }
                }
            }
        }
        output.push('\n');
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repomap_of_own_crate() {
        let map = generate_repomap(Path::new("."));
        assert!(map.contains("# Project Map:"));
        assert!(map.contains("rust"));
        // Should find our own public symbols
        assert!(map.contains("generate_repomap") || map.contains("scan_project"));
    }
}
