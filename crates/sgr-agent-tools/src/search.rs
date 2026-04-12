//! SearchTool — smart search with query expansion, fuzzy matching, and auto-expand.
//!
//! Core search logic without CRM annotations or content scanning.
//! For PAC1-specific behavior (CRM disambiguation, guard_content), wrap this tool.

use std::sync::Arc;

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use sgr_agent_core::agent_tool::{Tool, ToolError, ToolOutput, parse_args};
use sgr_agent_core::context::AgentContext;
use sgr_agent_core::schema::json_schema_for;

use crate::backend::FileBackend;
use crate::helpers::{backend_err, def_root, has_matches, unique_files_from_search};
use crate::trust::infer_trust;

pub struct SearchTool<B: FileBackend>(pub Arc<B>);

#[derive(Deserialize, JsonSchema)]
struct SearchArgs {
    /// Search root (file or directory path)
    #[serde(default = "def_root")]
    root: String,
    /// Regex pattern to search
    pattern: String,
    #[serde(default)]
    limit: i32,
}

/// Check if a pattern contains regex metacharacters.
pub fn is_regex(pattern: &str) -> bool {
    pattern.contains('.')
        || pattern.contains('*')
        || pattern.contains('[')
        || pattern.contains('(')
        || pattern.contains('|')
        || pattern.contains('+')
        || pattern.contains('?')
        || pattern.contains('{')
        || pattern.contains('\\')
}

/// Expand a search query into variants for auto-retry.
/// "John Smith" -> ["John Smith", "Smith John", "Smith", "John"]
pub fn expand_query(pattern: &str) -> Vec<String> {
    if is_regex(pattern) || pattern.trim().is_empty() {
        return vec![pattern.to_string()];
    }

    let words: Vec<&str> = pattern.split_whitespace().collect();
    if words.len() <= 1 {
        return vec![pattern.to_string()];
    }

    let mut variants = vec![pattern.to_string()];
    if words.len() == 2 {
        variants.push(format!("{} {}", words[1], words[0]));
    }
    if let Some(last) = words.last() {
        variants.push(last.to_string());
    }
    variants.push(words[0].to_string());
    variants
}

/// Generate a fuzzy regex for a short word: allow 1-char substitution at each position.
pub fn fuzzy_regex(word: &str) -> Option<String> {
    let w = word.trim();
    if w.len() < 3 || w.len() > 12 || is_regex(w) {
        return None;
    }
    let chars: Vec<char> = w.chars().collect();
    let alts: Vec<String> = (0..chars.len())
        .map(|i| {
            let mut s = String::new();
            for (j, c) in chars.iter().enumerate() {
                if j == i {
                    s.push('.');
                } else {
                    s.push(*c);
                }
            }
            s
        })
        .collect();
    Some(format!("(?i)({})", alts.join("|")))
}

/// Smart search: try original, then expanded variants, then fuzzy as last resort.
pub async fn smart_search<B: FileBackend>(
    backend: &B,
    root: &str,
    pattern: &str,
    limit: i32,
) -> anyhow::Result<String> {
    let result = backend.search(root, pattern, limit).await?;
    if has_matches(&result) {
        return Ok(result);
    }

    let variants = expand_query(pattern);
    for variant in &variants[1..] {
        let r = backend.search(root, variant, limit).await?;
        if has_matches(&r) {
            return Ok(r);
        }
    }

    let words: Vec<&str> = pattern.split_whitespace().collect();
    let target = words.last().unwrap_or(&pattern);
    if let Some(fuzzy) = fuzzy_regex(target) {
        let r = backend.search(root, &fuzzy, limit).await?;
        if has_matches(&r) {
            return Ok(r);
        }
    }

    // Levenshtein fallback on directory listing filenames
    if !is_regex(pattern) && pattern.len() >= 3 {
        if let Ok(listing) = backend.list(root).await {
            let query_lower = pattern.to_lowercase();
            let mut best_match: Option<(String, f64)> = None;
            for line in listing.lines() {
                let filename = line.trim().trim_end_matches('/');
                if filename.is_empty() || filename.starts_with('$') {
                    continue;
                }
                let name_part = filename.rsplit('.').last().unwrap_or(filename);
                let name_lower = name_part.to_lowercase().replace('-', " ").replace('_', " ");
                let score = strsim::normalized_levenshtein(&query_lower, &name_lower);
                if score > 0.7 && best_match.as_ref().map_or(true, |b| score > b.1) {
                    best_match = Some((format!("{}/{}", root, filename), score));
                }
            }
            if let Some((path, _score)) = best_match {
                let r = backend
                    .search(
                        root,
                        path.rsplit('/')
                            .next()
                            .unwrap_or(&path)
                            .replace(".md", "")
                            .as_str(),
                        limit,
                    )
                    .await?;
                if has_matches(&r) {
                    return Ok(r);
                }
            }
        }
    }

    Ok(result)
}

/// Auto-expand search results: if <=10 unique files, append full file content.
pub async fn auto_expand_search<B: FileBackend>(backend: &B, search_output: String) -> String {
    let files = unique_files_from_search(&search_output, 10);
    if files.is_empty() || files.len() > 10 {
        return search_output;
    }

    let mut expanded = search_output;
    for path in &files {
        if let Ok(content) = backend.read(path, false, 0, 0).await {
            let trust = infer_trust(path);
            let capped: String = content.lines().take(200).collect::<Vec<_>>().join("\n");
            expanded.push_str(&format!("\n\n--- {} [{}] ---\n{}", path, trust, capped));
        }
    }
    expanded
}

#[async_trait::async_trait]
impl<B: FileBackend> Tool for SearchTool<B> {
    fn name(&self) -> &str {
        "search"
    }
    fn description(&self) -> &str {
        "Search file contents with regex pattern. Smart search: auto-retries with name variants \
         (surname, first name) and fuzzy matching if no results. Auto-expands full file content \
         when <=10 files match. Output ends with [N matching lines] — use this count directly \
         for 'how many' queries instead of reading and counting manually."
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn parameters_schema(&self) -> Value {
        json_schema_for::<SearchArgs>()
    }
    async fn execute(&self, args: Value, ctx: &mut AgentContext) -> Result<ToolOutput, ToolError> {
        self.execute_readonly(args, ctx).await
    }
    async fn execute_readonly(
        &self,
        args: Value,
        _ctx: &AgentContext,
    ) -> Result<ToolOutput, ToolError> {
        let a: SearchArgs = parse_args(&args)?;
        let raw = smart_search(&*self.0, &a.root, &a.pattern, a.limit)
            .await
            .map_err(backend_err)?;
        let expanded = auto_expand_search(&*self.0, raw).await;
        let match_count = expanded
            .lines()
            .filter(|l| !l.is_empty() && !l.starts_with("$ "))
            .count();
        Ok(ToolOutput::text(format!(
            "{}\n\n[{} matching lines]",
            expanded, match_count
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_query_single_word() {
        assert_eq!(expand_query("Smith"), vec!["Smith"]);
    }

    #[test]
    fn expand_query_two_words() {
        let v = expand_query("John Smith");
        assert_eq!(v[0], "John Smith");
        assert_eq!(v[1], "Smith John");
        assert!(v.contains(&"Smith".to_string()));
        assert!(v.contains(&"John".to_string()));
    }

    #[test]
    fn expand_query_regex_unchanged() {
        assert_eq!(expand_query("(?i)test"), vec!["(?i)test"]);
    }

    #[test]
    fn fuzzy_regex_basic() {
        let r = fuzzy_regex("Smith").unwrap();
        assert!(r.starts_with("(?i)("));
        assert!(r.contains(".mith"));
        assert!(r.contains("Smit."));
    }

    #[test]
    fn fuzzy_regex_too_short() {
        assert!(fuzzy_regex("ab").is_none());
    }

    #[test]
    fn fuzzy_regex_too_long() {
        assert!(fuzzy_regex("abcdefghijklm").is_none());
    }

    #[test]
    fn fuzzy_regex_skips_regex() {
        assert!(fuzzy_regex("test.*foo").is_none());
    }

    #[test]
    fn is_regex_detects_metacharacters() {
        assert!(is_regex("test.*"));
        assert!(is_regex("test[0-9]"));
        assert!(is_regex("a|b"));
        assert!(!is_regex("John Smith"));
        assert!(!is_regex("simple"));
    }
}
