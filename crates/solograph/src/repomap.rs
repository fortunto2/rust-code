//! Generate a project map — concise summary of files and key symbols.
//!
//! Matches SoloGraph MCP's `codegraph_repomap` approach:
//! - Rank files by "importance" (symbol count as proxy for graph degree)
//! - Show actual code signatures (not just names)
//! - YAML-ish output format optimized for LLM context

use std::path::Path;

use crate::scanner::scan_project;
use crate::symbols::{extract_symbols, Symbol, SymbolKind};

/// Generate a YAML-style repomap for a project directory.
///
/// Files are ranked by symbol count (proxy for graph connectivity).
/// Top files show signatures extracted from source code.
pub fn generate_repomap(root: &Path) -> String {
    generate_repomap_with_limit(root, 20)
}

/// Generate repomap with custom file limit.
pub fn generate_repomap_with_limit(root: &Path, max_files: usize) -> String {
    let stats = scan_project(root);

    // Collect files with their symbols
    let mut ranked: Vec<RankedFile> = Vec::new();

    for fi in &stats.files {
        if !matches!(fi.language, "rust" | "python" | "typescript" | "javascript") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&fi.path) else {
            continue;
        };
        let symbols = extract_symbols(&fi.path, &source);
        if symbols.is_empty() {
            continue;
        }

        let rel = fi.path.strip_prefix(root).unwrap_or(&fi.path);
        let path_str = rel.to_str().unwrap_or("?").to_string();

        // Score: public symbols count + total symbols / 2
        // Proxy for graph degree (hub files define more symbols)
        let pub_count = symbols.iter().filter(|s| s.public).count();
        let score = pub_count * 2 + symbols.len();

        ranked.push(RankedFile {
            path: path_str,
            lines: fi.lines,
            symbols,
            source,
            score,
        });
    }

    // Sort by score descending
    ranked.sort_by(|a, b| b.score.cmp(&a.score));
    ranked.truncate(max_files);

    // Format output
    let mut out = String::new();

    let project_name = root
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "project".into());

    out.push_str(&format!(
        "# Project: {}\n# {} files, {} lines, languages: {}\n\n",
        project_name,
        stats.files.len(),
        stats.total_lines,
        stats
            .languages
            .iter()
            .map(|(l, c)| format!("{}({})", l, c))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    for rf in &ranked {
        out.push_str(&format!("{}:\n", rf.path));

        // Show public symbols with signatures
        let public: Vec<&Symbol> = rf.symbols.iter().filter(|s| s.public).collect();
        if public.is_empty() {
            continue;
        }

        out.push_str("  symbols:\n");
        for sym in public.iter().take(12) {
            let sig = extract_signature(&rf.source, sym);
            out.push_str(&format!("    - {}: {}", kind_str(&sym.kind), sym.name));
            if let Some(sig) = sig {
                out.push_str(&format!("  # {}", sig));
            }
            out.push_str(&format!(" (L{})\n", sym.line));
        }
        if public.len() > 12 {
            out.push_str(&format!("    # ... +{} more\n", public.len() - 12));
        }
    }

    out
}

/// Generate a compact context map for LLM injection.
///
/// - Compact project header (files, languages, LOC)
/// - Top files listed by name only (no symbols) — ~10 lines
/// - Full symbols only for `changed_files` (e.g. from `git status`)
///
/// Much smaller than full repomap — ideal for ephemeral per-call injection.
pub fn generate_context_map(root: &Path, changed_files: &[String]) -> String {
    let stats = scan_project(root);

    // Collect all ranked files
    let mut ranked: Vec<RankedFile> = Vec::new();

    for fi in &stats.files {
        if !matches!(fi.language, "rust" | "python" | "typescript" | "javascript") {
            continue;
        }
        let Ok(source) = std::fs::read_to_string(&fi.path) else {
            continue;
        };
        let symbols = extract_symbols(&fi.path, &source);
        if symbols.is_empty() {
            continue;
        }

        let rel = fi.path.strip_prefix(root).unwrap_or(&fi.path);
        let path_str = rel.to_str().unwrap_or("?").to_string();

        let pub_count = symbols.iter().filter(|s| s.public).count();
        let score = pub_count * 2 + symbols.len();

        ranked.push(RankedFile {
            path: path_str,
            lines: fi.lines,
            symbols,
            source,
            score,
        });
    }

    ranked.sort_by(|a, b| b.score.cmp(&a.score));

    // --- Header ---
    let project_name = root
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "project".into());

    let mut out = String::new();
    out.push_str(&format!(
        "# Project: {}\n# {} files, {} lines, languages: {}\n",
        project_name,
        stats.files.len(),
        stats.total_lines,
        stats
            .languages
            .iter()
            .map(|(l, c)| format!("{}({})", l, c))
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // --- Top files (names only, no symbols) ---
    out.push_str("\n# Key files (by symbol density):\n");
    for rf in ranked.iter().take(10) {
        out.push_str(&format!("  - {} ({} lines)\n", rf.path, rf.lines));
    }

    // --- Changed files with full symbols ---
    if !changed_files.is_empty() {
        out.push_str("\n# Changed files (detailed):\n");
        for changed in changed_files {
            // Normalize path for matching
            let changed_normalized = changed.trim_start_matches("./");
            if let Some(rf) = ranked.iter().find(|rf| rf.path == changed_normalized) {
                out.push_str(&format!("{}:\n", rf.path));
                let public: Vec<&Symbol> = rf.symbols.iter().filter(|s| s.public).collect();
                if public.is_empty() {
                    continue;
                }
                out.push_str("  symbols:\n");
                for sym in public.iter().take(12) {
                    let sig = extract_signature(&rf.source, sym);
                    out.push_str(&format!("    - {}: {}", kind_str(&sym.kind), sym.name));
                    if let Some(sig) = sig {
                        out.push_str(&format!("  # {}", sig));
                    }
                    out.push_str(&format!(" (L{})\n", sym.line));
                }
                if public.len() > 12 {
                    out.push_str(&format!("    # ... +{} more\n", public.len() - 12));
                }
            }
        }
    }

    out
}

struct RankedFile {
    path: String,
    #[allow(dead_code)]
    lines: usize,
    symbols: Vec<Symbol>,
    source: String,
    score: usize,
}

/// Extract a cleaned-up signature from source at the symbol's line.
fn extract_signature(source: &str, sym: &Symbol) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let idx = sym.line.checked_sub(1)?;
    let line = lines.get(idx)?;
    let trimmed = line.trim();

    if trimmed.is_empty() {
        return None;
    }

    // For decorators (Python @, Rust #[]), merge with next non-decorator line
    let mut sig = trimmed.to_string();
    if sig.starts_with('@') || sig.starts_with("#[") {
        if let Some(next) = lines.get(idx + 1) {
            let next_trimmed = next.trim();
            if !next_trimmed.starts_with('@') && !next_trimmed.starts_with("#[") {
                sig = format!("{} {}", sig, next_trimmed);
            }
        }
    }

    // Truncate long signatures (e.g. generics, where clauses)
    if sig.len() > 120 {
        sig.truncate(117);
        sig.push_str("...");
    }

    // Strip body starters
    let sig = sig
        .trim_end_matches('{')
        .trim_end_matches(':')
        .trim_end()
        .to_string();

    Some(sig)
}

fn kind_str(kind: &SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function => "fn",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Trait => "trait",
        SymbolKind::Impl => "impl",
        SymbolKind::Const => "const",
        SymbolKind::Static => "static",
        SymbolKind::TypeAlias => "type",
        SymbolKind::Mod => "mod",
        SymbolKind::Class => "class",
        SymbolKind::Method => "method",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repomap_of_own_crate() {
        let map = generate_repomap(Path::new("."));
        assert!(map.contains("# Project:"));
        assert!(map.contains("rust"));
        // Should find our own public symbols with signatures
        assert!(map.contains("generate_repomap"));
        // Should be YAML-ish format
        assert!(map.contains("symbols:"));
    }

    #[test]
    fn repomap_has_signatures() {
        let map = generate_repomap(Path::new("."));
        // Should contain fn/struct/etc markers
        assert!(
            map.contains("fn:") || map.contains("struct:") || map.contains("enum:"),
            "repomap should contain kind markers"
        );
        // Should contain line references
        assert!(map.contains("(L"), "repomap should contain line references");
    }

    #[test]
    fn context_map_compact() {
        let map = generate_context_map(Path::new("."), &[]);
        // Should have header
        assert!(map.contains("# Project:"));
        // Should have key files list
        assert!(map.contains("# Key files"));
        // Should NOT have full symbols (no changed files)
        assert!(!map.contains("# Changed files"));
        // Should be much shorter than full repomap
        let full = generate_repomap(Path::new("."));
        assert!(
            map.len() < full.len(),
            "context_map ({}) should be shorter than full repomap ({})",
            map.len(),
            full.len()
        );
    }

    #[test]
    fn context_map_with_changed_files() {
        let map = generate_context_map(Path::new("."), &["src/repomap.rs".to_string()]);
        assert!(map.contains("# Changed files"));
        assert!(map.contains("repomap.rs"));
        // Changed file should have symbols
        assert!(map.contains("symbols:"));
    }

    #[test]
    fn signature_extraction() {
        let source = "pub fn hello(name: &str) -> String {\n    format!(\"hi {}\", name)\n}\n";
        let sym = Symbol {
            name: "hello".into(),
            kind: SymbolKind::Function,
            line: 1,
            public: true,
        };
        let sig = extract_signature(source, &sym);
        assert_eq!(sig.unwrap(), "pub fn hello(name: &str) -> String");
    }

    #[test]
    fn signature_truncates_long_lines() {
        let long_fn = format!(
            "pub fn very_long_function({}) -> Result<(), Error> {{",
            "a: &str, ".repeat(20)
        );
        let sym = Symbol {
            name: "very_long_function".into(),
            kind: SymbolKind::Function,
            line: 1,
            public: true,
        };
        let sig = extract_signature(&long_fn, &sym).unwrap();
        assert!(sig.len() <= 120);
        assert!(sig.ends_with("..."));
    }
}
